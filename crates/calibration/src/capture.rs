// NOTE(port): legacy/pre-rewrite rt-stack/crates/calibration/src/capture.rs —— 原样迁入（T62 UMIK 采集）；
// 仅 import 适配（v1 core/rt-host 类型改走 crate::legacy_* 契约替身）与测试注册路径，算法/阈值/字段语义不改。
//! [NONRT] UMIK 独立 USB ADC 1ch 采集（WBS T62，deps T61/T19/T22） ——本文件是 WBS T62 写域。
//!
//! 职责：把校准 daemon 的 UMIK-1 独立 USB ADC 单通道声学采集做成可离线验证的采集层：
//! raw 落盘字节（1ch dense）、repeat（每遍扫频起点采集样本）、timebase（render frame 与
//! UMIK capture sample 的仿射映射 + clock_ppm/uncertainty/显著性），并把可选的 MOTU 电学
//! reference 作为另一个独立设备/通道采集。漂移与公共相位基准不混设备：一个测量会话的 drift
//! source 与 common phase reference 必须是同一设备时钟域；跨设备组合一律拒收（见设备分离规则）。
//!
//! 契约来源：
//! - WBS T62 行：crates/calibration/src/capture.rs —— UMIK 独立 USB ADC 1ch 采集 raw/repeat/
//!   timebase；MOTU 可选电学 reference 另 channel；漂移与公共相位基准不混设备；验收命令
//!   cargo test -p khaen-calibration umik_capture。
//! - WBS §1（唯一 P1 完成定义）、§3（校准设备语义：UMIK 是独立 USB ADC 单通道，MOTU 输出扫频；
//!   MOTU 可选电学 reference 为另独立通道，不是 MOTU 采 UMIK）、§6（executed test_count>0，
//!   空 stub/0 tests 不能 PASS）、§7（本模块不打开设备）。
//! - DSP-HOST-DECISION §2.2/§2.3：daemon 经新 host 用 MOTU 输出扫频，UMIK 独立 USB ADC 1ch
//!   声学采集并保存 raw/source/slot/repeat metadata；MOTU 可选电学 reference 用另一个明确通道
//!   独立采集；新 daemon 关联公共 render frame/slot、UMIK sample_index 与漂移/相位基准，
//!   记 MOTU frame/UMIK ADC/PTP/mono 关系、重复起点与 ppm 不确定度/有效性/显著性。
//! - calibration-spec/ACCEPTANCE：
//!   §0.1.2 多声道/环绕听音位用 UMIK-1 90° 校准文件、麦轴垂直朝天花板（0° 只用于单只近场），
//!   报告必须记录 mic{model, serial, cal_file, sha256, orientation}；
//!   §0.2.2 UMIK 是异步 USB 时钟，必须同时报告 clock_ppm 与 clock_ppm_uncertainty_ppm，
//!   只有 |ppm| > 2 sigma 才可称漂移并允许 drift-correct 重采样；|ppm| <= 100 ppm 只是 sanity
//!   上界不是漂移证据；N=1 不可估计漂移、不得重采样；
//!   §0.2.4 补偿后 10 kHz 残余相位 <= 15 度的时间跨度必须写死（默认 10.4 s 且 sigma_ppm<=0.4，
//!   或 >=5 遍 / >=60 s），未达重采条件 -> NOT_APPLICABLE；
//!   §0.5 采集峰值 <= -1 dBFS、无数字削波，报告 capture_peak_dbfs。
//! - rt-stack/spec/mimo-t6.md FC-04/FC-05/FC-06：公共 D 与相位基准预声明、render frame/slot 与
//!   capture sample_index 映射方法声明、clock_ppm + clock_ppm_uncertainty_ppm 的 2 sigma 口径。
//! - rt-stack/spec/core-v1.md §3.2（48k/96k、B 属于 {64,128}）、§5.4（OwnerCalibration 独占打开
//!   设备；本模块只处理已采到的数据，不打开 ALSA/不抢 owner）、§11.9（设备格式/hwparams 语义）。
//! - T61 crates/calibration/src/sweep.rs：SweepTimeline（公共 render frame/slot、
//!   repeat_render_frames、actual_stimulus）是本模块的时轴输入。
//! - T19 crates/rt-host/src/clock.rs：ClockObservation/PpmEstimator/PpmEstimate 的 ppm+sigma
//!   语义；本模块 repeat-interval 主估计之外，还用 T19 PpmEstimator 做交叉复核。
//! - T22 crates/rt-host/src/format.rs：设备格式解码、nonfinite sanitize 与 |x|>1 clip 计数语义；
//!   MOTU 电学 reference 是双工设备，直接走 format::convert_capture 解码。UMIK 是 capture-only
//!   设备，不能套用 T22 duplex FormatConfig（其 validate 要求 n_out>0）；UMIK 单通道解码按 T22
//!   同一语义实现（见 decode_mono_samples）。
//! - T48 crates/modal-design/src/ingest.rs：消费者 schema MicInfo/MicClock/CaptureFile/
//!   SampleFormat/DriftCorrection/TimebaseDiag；本模块直接产出这些类型（T63/T49 不再二次解释）。
//!
//! 设备分离规则（WBS T62 漂移与公共相位基准不混设备）：
//!
//! | purpose | drift source | common phase reference | 可选 MOTU 电学 reference |
//! | --- | --- | --- | --- |
//! | AcousticRoom（T2/T4/T6） | UMIK repeat grid | UMIK direct onset | 允许，但只能是 auxiliary：独立设备/通道，记录峰值/削波，不参与 UMIK 的 drift 或公共相位基准 |
//! | ElectricalLoopback（T1） | MOTU electrical loopback | MOTU electrical loopback | 就是主 reference（另 channel），UMIK 不参与 |
//!
//! 硬规则（ReferencePlan::validate 实现，负向用例见 tests/T62）：
//! 1. drift source 与 common phase reference 必须属于同一设备时钟域（同时 UMIK 或同时 MOTU）；
//! 2. 声学会话两者都必须是 UMIK（UMIK 是唯一声学 ADC）；MOTU 电学 reference 只能是 auxiliary；
//! 3. 电学环回会话两者都必须是 MOTU（不与 UMIK 混）；
//! 4. MOTU 电学 reference 必须是独立设备（device id 与 UMIK 不同），否则拒收；
//! 5. 分析时 CapturedStream 的 device/session/epoch/graph 必须与 plan/grid 一致，否则拒收。
//!
//! 实现边界（NOT_RUN）：
//! - T67 符号自测（calibration-spec/MICS.md）尚未执行：声学报告 mic_calibration 恒为
//!   NOT_RUN / INCOMPLETE（accuracy_db=null），禁止声称 ±3 dB；normal/inverted 自测接线属 T67。
//! - 不打开 ALSA / 不写 /dev/snd / 不启动外部进程 / 不抢 OwnerCalibration（真实读块由 T17
//!   backend + T26 接入后调用 UmikCapture::push_block / ElectricalReferenceCapture::push_block）。
//! - 不做 Farina 反卷积、不做绝对传播延迟（T49 公共时轴/相位重建）、不做 SNR 带限统计
//!   （T63 quality gate）、不做拟合/求解/应用（T63-T66）。
//! - onset 只用于 repeat drift/relative timing（阈值插值法），不是绝对到达时刻；报告显式记录方法。
//! - 无真实 UMIK/MOTU 采集、无板端、无 RT deadline 证据（见 evidence/T62/MANIFEST.md 未测项）。
//!
//! 测试注册（T06 纪律）：私有测试在 crates/calibration/tests/T62/umik_capture.rs，由本文件末尾
//! #[cfg(test)] mod t62_tests { include!(...) } 编进 lib 单测；不要再注册进
//! tests/public_registration.rs（会重复执行）。运行：cargo test -p khaen-calibration umik_capture。

use std::fmt;
use std::fmt::Write as _;

use khaen_core::rt::{BLOCK_SIZES, SAMPLE_RATES_HZ};
use khaen_modal_design::ingest::{
    CaptureFile, DriftCorrection, MicClock, MicInfo, SampleFormat, TimebaseDiag,
};

use crate::job::sha256_hex;
use crate::legacy_clock::{ClockObservation, PpmEstimate, PpmEstimator};
use crate::legacy_contract::sample_format;
use crate::legacy_format as format;
use crate::legacy_format::{FormatConfig, PeriodLayout};
use crate::sweep::{SweepTimeline, PLACEHOLDER_GRAPH_ID};

// ---------------------------------------------------------------------------
// 常量（ACCEPTANCE / WBS T62 / core-v1 / T19）
// ---------------------------------------------------------------------------

/// 采集报告 schema（raw/repeat/timebase 证据包）。
pub const UMIK_CAPTURE_SCHEMA: &str = "khaen-umik-capture/v1";
/// 时基报告 schema（render frame 与 capture sample 映射 + clock）。
pub const UMIK_TIMEBASE_SCHEMA: &str = "khaen-umik-timebase/v1";

/// P1 口径：UMIK 是独立 USB ADC 单通道采集（DSP-HOST-DECISION §2.2）。
pub const UMIK_REQUIRED_CHANNELS: u16 = 1;
/// UMIK-1 硬件只支持 48 kHz（96k 需换麦克风，旧 calibrate 对 --mic umik 且 rate!=48000 直接报错）。
pub const UMIK1_NATIVE_SAMPLE_RATE_HZ: u32 = 48_000;
/// UMIK 时钟域声明（P1 ingest 只接受 usb；与 MOTU 同源时禁止冒称独立 ADC）。
pub const UMIK_CLOCK_DOMAIN_USB: &str = "usb";
/// 多声道/环绕听音位的 UMIK 朝向（90° 校准文件，麦轴垂直朝天花板；ACCEPTANCE §0.1.2）。
// NOTE(port): 原为 `pub use khaen_cal_dsp::capture::UMIK_ORIENTATION_VERTICAL;`；v2 无
// khaen-cal-dsp crate，按原定义字面值就地保留（公开路径不变，positions.rs/T63 仍引用此常量）。
pub const UMIK_ORIENTATION_VERTICAL: &str = "90deg";
/// 单只近场/轴上的 UMIK 朝向（0° 校准文件；ACCEPTANCE §0.1.2）。
pub const UMIK_ORIENTATION_AXIAL: &str = "0deg";

/// T67 符号自测状态（MICS.md：UMIK/MA1 符号语义在 T67 自测前禁止声称 ±3 dB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignSelfTestStatus {
    /// 未执行（T67 前默认；校准精度声明必须保持 INCOMPLETE）。
    NotRun,
    /// 已执行且符号约定确认（同一测量 normal/inverted 自测通过）。
    Passed,
    /// 已执行但符号约定不符（不得用于校正，也不得声称精度）。
    Failed,
}

impl SignSelfTestStatus {
    /// 机器名（报告 JSON/evidence）。
    pub const fn as_str(self) -> &'static str {
        match self {
            SignSelfTestStatus::NotRun => CAL_SIGN_SELF_TEST_NOT_RUN,
            SignSelfTestStatus::Passed => "PASSED",
            SignSelfTestStatus::Failed => "FAILED",
        }
    }
}

/// 符号自测状态字面值：未执行（T67 前默认；禁止声称 ±3 dB）。
pub const CAL_SIGN_SELF_TEST_NOT_RUN: &str = "NOT_RUN";
/// 校准精度声明状态：INCOMPLETE（符号自测未完成；禁止声称 ±3 dB）。
pub const CAL_ACCURACY_INCOMPLETE: &str = "INCOMPLETE";
/// 校准精度声明状态：READY（只有 T67 符号自测通过后才可出现）。
pub const CAL_ACCURACY_READY: &str = "READY";
/// 校准精度声明状态：FAIL（符号自测不符）。
pub const CAL_ACCURACY_FAIL: &str = "FAIL";

/// 麦克风校准精度声明（MICS.md：T67 符号自测前保持 INCOMPLETE，禁止声称 ±3 dB）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicCalibrationVerification {
    /// T67 符号自测状态。
    pub sign_self_test: SignSelfTestStatus,
    /// 允许声明的校准精度（dB，正数）；None = INCOMPLETE/FAIL，不得声称精度。
    pub accuracy_db: Option<f64>,
}

impl MicCalibrationVerification {
    /// T67 未执行时的如实状态：NOT_RUN + INCOMPLETE（禁止 ±3 dB）。
    pub const fn not_run() -> Self {
        MicCalibrationVerification {
            sign_self_test: SignSelfTestStatus::NotRun,
            accuracy_db: None,
        }
    }

    /// 精度声明状态字面值（INCOMPLETE/READY/FAIL）。
    pub const fn accuracy_status(&self) -> &'static str {
        match self.sign_self_test {
            SignSelfTestStatus::NotRun => CAL_ACCURACY_INCOMPLETE,
            SignSelfTestStatus::Failed => CAL_ACCURACY_FAIL,
            SignSelfTestStatus::Passed => match self.accuracy_db {
                Some(_) => CAL_ACCURACY_READY,
                None => CAL_ACCURACY_INCOMPLETE,
            },
        }
    }

    /// 允许声称的精度（dB）；T67 符号自测完成前恒为 None（不得声称 ±3 dB）。
    pub const fn accuracy_claim_db(&self) -> Option<f64> {
        self.accuracy_db
    }
}

/// 漂移显著性因子：|ppm| > 2 sigma 才算漂移（ACCEPTANCE §0.2.2 / mimo-t6 FC-06）。
pub const DRIFT_SIGNIFICANCE_SIGMA: f64 = 2.0;
/// |ppm| <= 100 只是 sanity 上界，不是漂移证据（ACCEPTANCE §0.2.2）。
pub const DRIFT_SANITY_LIMIT_PPM: f64 = 100.0;
/// ppm 1 sigma 下限（避免干净数据给出物理上不可信的超小 sigma；T19 SIGMA_FLOOR_PPB=50 同量级）。
pub const PPM_SIGMA_FLOOR_PPM: f64 = 0.05;
/// clock 估计有效性上界：sigma 超过该值即使点估计有限也不可用于重采样决策。
pub const CLOCK_SIGMA_MAX_PPM: f64 = 50.0;
/// onset 拟合残差 RMS 上界（capture 样本）；超限标 clock_ppm_ok=false（不静默放行）。
pub const ONSET_RESIDUAL_MAX_SAMPLES: f64 = 96.0;

/// 默认 onset 搜索半窗（秒）：覆盖 USB/DAC 延迟与传播延迟（0.25 s = 12000 samples @48k）。
pub const DEFAULT_ONSET_SEARCH_HALF_WINDOW_SECONDS: f64 = 0.25;
/// onset 搜索半窗下限（秒）。
pub const MIN_ONSET_SEARCH_HALF_WINDOW_SECONDS: f64 = 0.02;
/// onset 搜索半窗上限（秒）。
pub const MAX_ONSET_SEARCH_HALF_WINDOW_SECONDS: f64 = 2.0;
/// 默认 onset 前向搜索窗（秒）：20 Hz 起扫 + 房间低频滚降时，带限 LF 包络第一处过阈
/// 可能落在 expected 之后约 0.6 s；只搜 ±half 会把真实起点排除在窗外（T62 NoOnsets 根因）。
pub const DEFAULT_ONSET_SEARCH_FORWARD_SECONDS: f64 = 1.0;
/// 前向搜索窗下限（秒）。
pub const MIN_ONSET_SEARCH_FORWARD_SECONDS: f64 = 0.05;
/// 前向搜索窗上限（秒）。
pub const MAX_ONSET_SEARCH_FORWARD_SECONDS: f64 = 2.0;
/// 默认 onset 阈值：参考包络（带限 LF）峰值的 10%。20 Hz 起扫 + 房间低频滚降时，起始
/// 20–50 Hz 段比峰值低 >10 dB；0.3 会把阈值抬到起始段之上（旧 T62 NoOnsets 根因）。
pub const DEFAULT_ONSET_THRESHOLD_FRACTION: f64 = 0.1;
/// onset 阈值下限（低于此比例会被噪声/回响尾触发）。
pub const MIN_ONSET_THRESHOLD_FRACTION: f64 = 0.01;
/// onset 阈值上限（高于此比例会落在 ramp 陡段，电平/回响敏感）。
pub const MAX_ONSET_THRESHOLD_FRACTION: f64 = 0.9;
/// 默认 onset 噪声底门（dB）：阈值还须比带限 LF 包络的稳健噪声底高这么多（双门）。
pub const DEFAULT_ONSET_NOISE_GATE_DB: f64 = 6.0;
/// onset 噪声底门下限（dB）。
pub const MIN_ONSET_NOISE_GATE_DB: f64 = 0.0;
/// onset 噪声底门上限（dB）。
pub const MAX_ONSET_NOISE_GATE_DB: f64 = 40.0;
/// 带限 LF onset 包络的下边带（Hz）：避开 20 Hz 极低能量，同时保留 sweep 起始 LF 段。
pub const ONSET_LF_BAND_LOW_HZ: f64 = 40.0;
/// 带限 LF onset 包络的上边带（Hz）。
pub const ONSET_LF_BAND_HIGH_HZ: f64 = 200.0;
/// onset 峰值参考窗前向长度（秒）：与 sweep 起始频率解耦的固定值。
/// 20 Hz 起扫时扫频约 1.67 s 才到 200 Hz（LF 带峰值），2.0 s 覆盖带内峰值。
pub const ONSET_REFERENCE_LOOKAHEAD_SECONDS: f64 = 2.0;
/// onset 包络平滑（秒）：给阈值过零提供稳定交叉点。
pub const ONSET_ENVELOPE_TAU_SECONDS: f64 = 0.002;
/// 带限 LF 包络过阈后的保持时间（秒）：抑制单点噪声毛刺/孤立 pop 误检（pop 只有几 ms，
/// 而 20 Hz 起扫的 LF 包络一旦起来会持续 >1 s）。100 ms 是保守的瞬态拒绝门。
pub const ONSET_LF_HOLD_SECONDS: f64 = 0.100;
/// 宽带包络（精化起点）过阈后的保持时间（秒）。
pub const ONSET_WIDE_HOLD_SECONDS: f64 = 0.005;
/// 包络峰值下限（低于此视为无信号）。
pub const ONSET_ENVELOPE_MIN_PEAK: f64 = 1.0e-9;
/// 相邻 repeat 间隔容差（比例）。
pub const ONSET_INTERVAL_TOLERANCE_FRACTION: f64 = 0.0002;
/// 相邻 repeat 间隔容差（绝对样本下限）。
pub const ONSET_INTERVAL_TOLERANCE_SAMPLES: f64 = 8.0;

/// 采集总时长上限（秒）：避免误配置打满内存（真实 T2/T4 一次会话约 10-60 s）。
pub const MAX_CAPTURE_SECONDS: f64 = 600.0;
/// 单 block/chunk 帧数下限。
pub const MIN_CHUNK_FRAMES: u32 = 1;
/// 单 block/chunk 帧数上限。
pub const MAX_CHUNK_FRAMES: u32 = 16_384;
/// 无信号时的 dBFS 下限占位（JSON 用有限值；不算通过任何峰值门）。
pub const SILENCE_DBFS: f64 = -200.0;
/// 采集峰值门（ACCEPTANCE §0.5：峰值 <= -1 dBFS、无数字削波）。
pub const CAPTURE_PEAK_CEILING_DBFS: f64 = -1.0;

/// 残余相位 15 度判定的默认时间跨度（秒；ACCEPTANCE §0.2.4：3 遍 x 5 s + 槽间 0.2 s）。
pub const RESIDUAL_PHASE_DEFAULT_SPAN_SECONDS: f64 = 10.4;
/// 残余相位 15 度判定要求的 sigma_ppm（ACCEPTANCE §0.2.4）。
pub const RESIDUAL_PHASE_SIGMA_MAX_PPM: f64 = 0.4;
/// 残余相位 15 度判定的替代跨度：>=5 遍（ACCEPTANCE §0.2.4）。
pub const RESIDUAL_PHASE_MIN_REPEATS: u32 = 5;
/// 残余相位 15 度判定的替代跨度：>=60 s（ACCEPTANCE §0.2.4）。
pub const RESIDUAL_PHASE_MIN_SPAN_SECONDS: f64 = 60.0;
/// T19 交叉复核最少点数（PpmEstimator::MIN_FIT_SAMPLES 同为 4）。
pub const T19_CROSS_CHECK_MIN_POINTS: usize = 4;
/// 一个采集会话最多 repeat 数（与 T61 MAX_REPEATS 对齐）。
pub const MAX_REPEATS_PER_SLOT: u32 = 10;
/// 一个采集会话最多 slot 数（core-v1 §3.3 MAX_OUTPUT_ROLES）。
pub const MAX_REPEAT_SLOTS: usize = 16;

// ---------------------------------------------------------------------------
// 基础枚举
// ---------------------------------------------------------------------------

/// 采集流角色：UMIK 声学 ADC 或 MOTU 电学 reference。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStreamRole {
    /// UMIK 独立 USB ADC（声学，1ch）。
    UmikAdc,
    /// MOTU 电学 reference（另 channel，MOTU 双工设备输入通道）。
    MotuElectrical,
}

impl CaptureStreamRole {
    /// 机器名（报告/JSON）。
    pub const fn as_str(self) -> &'static str {
        match self {
            CaptureStreamRole::UmikAdc => "umik_adc",
            CaptureStreamRole::MotuElectrical => "motu_electrical",
        }
    }
}

/// 采集会话用途：声学房间测量（T2/T4/T6）或电学环回（T1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePurpose {
    /// T2/T4/T6：UMIK 声学采集；MOTU 电学 reference 只能 auxiliary。
    AcousticRoom,
    /// T1：MOTU 电学环回；UMIK 不参与。
    ElectricalLoopback,
}

impl CapturePurpose {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            CapturePurpose::AcousticRoom => "acoustic",
            CapturePurpose::ElectricalLoopback => "electrical_loopback",
        }
    }
}

/// 漂移（clock ppm）估计的设备域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimebaseDriftSource {
    /// UMIK repeat grid 相对 host render frame（声学会话）。
    UmikRepeatGrid,
    /// MOTU 电学环回（T1；播放与采集同为 MOTU 时钟域）。
    MotuElectricalLoopback,
}

impl TimebaseDriftSource {
    /// 设备角色（用于不混设备校验）。
    pub const fn role(self) -> CaptureStreamRole {
        match self {
            TimebaseDriftSource::UmikRepeatGrid => CaptureStreamRole::UmikAdc,
            TimebaseDriftSource::MotuElectricalLoopback => CaptureStreamRole::MotuElectrical,
        }
    }

    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            TimebaseDriftSource::UmikRepeatGrid => "umik_repeat_grid",
            TimebaseDriftSource::MotuElectricalLoopback => "motu_electrical_loopback",
        }
    }
}

/// 公共相位基准的设备域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommonPhaseReference {
    /// UMIK 直达声 onset 基准（声学会话；同一 UMIK 采集流）。
    UmikDirectOnset,
    /// MOTU 电学环回 reference 通道（T1；与 drift 同一 MOTU 设备时钟域）。
    MotuElectricalLoopback,
}

impl CommonPhaseReference {
    /// 设备角色（用于不混设备校验）。
    pub const fn role(self) -> CaptureStreamRole {
        match self {
            CommonPhaseReference::UmikDirectOnset => CaptureStreamRole::UmikAdc,
            CommonPhaseReference::MotuElectricalLoopback => CaptureStreamRole::MotuElectrical,
        }
    }

    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            CommonPhaseReference::UmikDirectOnset => "umik_direct_onset",
            CommonPhaseReference::MotuElectricalLoopback => "motu_electrical_loopback",
        }
    }
}

/// onset 检测方法（报告必须声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnsetMethod {
    /// 局部峰值比例阈值 + 线性插值（电平归一；不是绝对到达时刻）。
    ThresholdFraction,
    /// 带限 LF 包络阈值（40–200 Hz）+ 噪声底双门；宽带包络在同阈值上做起点精化。
    /// 修复 20 Hz 起扫 + 房间低频滚降时的 T62 NoOnsets（见 DESIGN）。
    BandlimitedLfThreshold,
}

impl OnsetMethod {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            OnsetMethod::ThresholdFraction => "threshold_fraction",
            OnsetMethod::BandlimitedLfThreshold => "bandlimited_lf_threshold",
        }
    }
}

/// 残余相位 15 度判定的跨度口径（ACCEPTANCE §0.2.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidualPhaseCriterion {
    /// 默认 10.4 s 跨度（3 遍 x 5 s + 0.2 s 槽间）且 sigma_ppm<=0.4。
    DefaultSpan10Point4,
    /// >=5 遍。
    FiveRepeats,
    /// >=60 s 数据。
    SixtySeconds,
}

impl ResidualPhaseCriterion {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ResidualPhaseCriterion::DefaultSpan10Point4 => "default_span_10_4s",
            ResidualPhaseCriterion::FiveRepeats => "five_repeats",
            ResidualPhaseCriterion::SixtySeconds => "sixty_seconds",
        }
    }
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 采集层错误（稳定 code 用于 REST/log/evidence；未知/越界一律拒绝，不静默降级）。
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureError {
    /// 配置非法（字段级消息）。
    InvalidConfig {
        /// 字段名。
        field: &'static str,
        /// 人读说明。
        detail: String,
    },
    /// 不支持的设备样本格式。
    UnsupportedSampleFormat {
        /// 角色。
        role: &'static str,
        /// 实际格式名。
        format: String,
    },
    /// 采集通道数不符（P1 UMIK 必须 1ch）。
    UnexpectedChannelCount {
        /// 角色。
        role: &'static str,
        /// 期望通道数。
        expected: u16,
        /// 实际通道数。
        actual: u16,
    },
    /// 采样率不被接受（UMIK-1 只支持 48k；runtime 只接受 48k/96k）。
    UnsupportedSampleRate {
        /// 实际采样率。
        actual: u32,
        /// 原因。
        detail: String,
    },
    /// T61 时间轴不满足本模块契约（slot/repeat/render frame）。
    InvalidTimeline {
        /// 细节。
        detail: String,
    },
    /// 采集 block 非法（长度/时间戳/序号）。
    InvalidBlock {
        /// block 序号。
        block: u64,
        /// 细节。
        detail: String,
    },
    /// block 时间戳不是严格递增。
    NonMonotonicStamp {
        /// 上一块 mono ns。
        previous_ns: u64,
        /// 本块 mono ns。
        actual_ns: u64,
        /// block 序号。
        block: u64,
    },
    /// 采集帧数超过上限。
    TooManyFrames {
        /// 上限。
        limit: u64,
        /// 请求后总数。
        requested: u64,
    },
    /// drift 与公共相位基准试图跨设备组合（WBS T62 禁止）。
    MixedReferenceDevices {
        /// 细节。
        detail: String,
    },
    /// MOTU 电学 reference 与 UMIK 不是独立设备。
    ReferenceNotIndependent {
        /// 设备串。
        device: String,
    },
    /// 整个采集没有任何有效 onset（不能做时基）。
    NoOnsets {
        /// 会话 slot 数。
        slots: usize,
        /// 检测器上下文（带限 LF 包络/阈值/搜索窗；失败原因必须可追溯）。
        detail: String,
    },
    /// clock 不可估计（有效间隔不足/无效），不能产出 diag。
    ClockNotEstimable {
        /// 有效间隔数。
        intervals: usize,
    },
}

impl CaptureError {
    /// 稳定错误码（REST/log 用）。
    pub const fn code(&self) -> &'static str {
        match self {
            CaptureError::InvalidConfig { .. } => "E_CAPTURE_CONFIG",
            CaptureError::UnsupportedSampleFormat { .. } => "E_CAPTURE_FORMAT",
            CaptureError::UnexpectedChannelCount { .. } => "E_CAPTURE_CHANNELS",
            CaptureError::UnsupportedSampleRate { .. } => "E_CAPTURE_RATE",
            CaptureError::InvalidTimeline { .. } => "E_CAPTURE_TIMELINE",
            CaptureError::InvalidBlock { .. } => "E_CAPTURE_BLOCK",
            CaptureError::NonMonotonicStamp { .. } => "E_CAPTURE_STAMP",
            CaptureError::TooManyFrames { .. } => "E_CAPTURE_TOO_MANY_FRAMES",
            CaptureError::MixedReferenceDevices { .. } => "E_CAPTURE_REF_MIXED_DEVICES",
            CaptureError::ReferenceNotIndependent { .. } => "E_CAPTURE_REF_NOT_INDEPENDENT",
            CaptureError::NoOnsets { .. } => "E_CAPTURE_NO_ONSETS",
            CaptureError::ClockNotEstimable { .. } => "E_CAPTURE_CLOCK",
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::InvalidConfig { field, detail } => {
                write!(f, "{}: invalid config field {field}: {detail}", self.code())
            }
            CaptureError::UnsupportedSampleFormat { role, format } => write!(
                f,
                "{}: unsupported sample format {format} for {role}",
                self.code()
            ),
            CaptureError::UnexpectedChannelCount {
                role,
                expected,
                actual,
            } => write!(
                f,
                "{}: {role} requires {expected} channel(s), got {actual}",
                self.code()
            ),
            CaptureError::UnsupportedSampleRate { actual, detail } => {
                write!(
                    f,
                    "{}: unsupported sample rate {actual}: {detail}",
                    self.code()
                )
            }
            CaptureError::InvalidTimeline { detail } => {
                write!(f, "{}: invalid timeline: {detail}", self.code())
            }
            CaptureError::InvalidBlock { block, detail } => {
                write!(f, "{}: invalid block {block}: {detail}", self.code())
            }
            CaptureError::NonMonotonicStamp {
                previous_ns,
                actual_ns,
                block,
            } => write!(
                f,
                "{}: block {block} mono_ns {actual_ns} <= previous {previous_ns}",
                self.code()
            ),
            CaptureError::TooManyFrames { limit, requested } => write!(
                f,
                "{}: capture would exceed {limit} frames (requested {requested})",
                self.code()
            ),
            CaptureError::MixedReferenceDevices { detail } => {
                write!(f, "{}: {detail}", self.code())
            }
            CaptureError::ReferenceNotIndependent { device } => write!(
                f,
                "{}: electrical reference device {device} is not independent from UMIK",
                self.code()
            ),
            CaptureError::NoOnsets { slots, detail } => write!(
                f,
                "{}: no valid onsets found across {slots} slot(s): {detail}",
                self.code()
            ),
            CaptureError::ClockNotEstimable { intervals } => write!(
                f,
                "{}: clock not estimable with {intervals} valid repeat interval(s)",
                self.code()
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

// ---------------------------------------------------------------------------
// 小工具（hex/JSON/数值）
// ---------------------------------------------------------------------------

fn is_lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_nonzero_sha256_hex(value: &str) -> bool {
    value.len() == 64 && is_lower_hex(value) && value.bytes().any(|b| b != b'0')
}

/// 校准文件名/相对路径（必填、已 trim、<=256 字符、无控制字符；不要求文件已存在）。
fn is_clean_cal_file(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= 256
        && !value.chars().any(char::is_control)
}

fn is_nonzero_graph_id(value: &str) -> bool {
    value.len() == 64 && is_lower_hex(value) && value.bytes().any(|b| b != b'0')
}

fn finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn dbfs_of(amplitude: f64) -> f64 {
    if amplitude > 0.0 {
        20.0 * amplitude.log10()
    } else {
        SILENCE_DBFS
    }
}

fn push_json_str(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            other if (other as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

fn push_json_number(out: &mut String, value: f64) {
    if value.is_finite() {
        let _ = write!(out, "{value}");
    } else {
        out.push_str("null");
    }
}

fn push_json_opt_number(out: &mut String, value: Option<f64>) {
    match value {
        Some(value) => push_json_number(out, value),
        None => out.push_str("null"),
    }
}

// ---------------------------------------------------------------------------
// 设备配置
// ---------------------------------------------------------------------------

/// UMIK 设备声明（P1：独立 USB ADC 单通道；ACCEPTANCE §0.1 报告字段）。
#[derive(Debug, Clone, PartialEq)]
pub struct UmikDeviceConfig {
    /// ALSA / 稳定设备 id（如 hw:UMIK1,0 或 by-id）。
    pub device: String,
    /// 型号（P1 UMIK-1）。
    pub model: String,
    /// 序列号（如 718-7696）。
    pub serial: String,
    /// 校准文件名/相对路径（必填；与 cal_sha256 指向同一份校准文件；听音位用 90° 文件）。
    pub cal_file: String,
    /// miniDSP 校准文件字节 sha256（64 lowercase hex、非全零）。
    pub cal_sha256: String,
    /// 朝向声明：多声道听音位 90deg（麦轴垂直朝天花板），0deg 仅单只近场（ACCEPTANCE §0.1.2）。
    pub orientation: String,
    /// 时钟域；P1 必须是 usb（UMIK 是独立 USB ADC，不与 MOTU 同源）。
    pub clock_domain: String,
    /// 采样率（UMIK-1 只支持 48 kHz）。
    pub sample_rate_hz: u32,
    /// 采集通道数（P1 必须 1）。
    pub channels: u16,
    /// 采集样本格式（S24_3LE / S32_LE / F32_LE）。
    pub sample_format: SampleFormat,
}

impl UmikDeviceConfig {
    /// UMIK-1 默认声明（48k / 1ch / S24_3LE / usb）。
    pub fn umik1(
        device: impl Into<String>,
        serial: impl Into<String>,
        cal_file: impl Into<String>,
        cal_sha256: impl Into<String>,
        orientation: impl Into<String>,
    ) -> Self {
        UmikDeviceConfig {
            device: device.into(),
            model: "UMIK-1".to_string(),
            serial: serial.into(),
            cal_file: cal_file.into(),
            cal_sha256: cal_sha256.into(),
            orientation: orientation.into(),
            clock_domain: UMIK_CLOCK_DOMAIN_USB.to_string(),
            sample_rate_hz: UMIK1_NATIVE_SAMPLE_RATE_HZ,
            channels: UMIK_REQUIRED_CHANNELS,
            sample_format: SampleFormat::S24_3LE,
        }
    }

    /// 严格校验（未知/越界全部拒绝）。
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.device.trim().is_empty() || self.device.chars().count() > 128 {
            return Err(CaptureError::InvalidConfig {
                field: "mic.device",
                detail: "must be non-empty and <=128 chars".to_string(),
            });
        }
        if self.model.trim().is_empty() {
            return Err(CaptureError::InvalidConfig {
                field: "mic.model",
                detail: "must be non-empty".to_string(),
            });
        }
        if self.serial.trim().is_empty() {
            return Err(CaptureError::InvalidConfig {
                field: "mic.serial",
                detail: "must be non-empty".to_string(),
            });
        }
        if !is_clean_cal_file(&self.cal_file) {
            return Err(CaptureError::InvalidConfig {
                field: "mic.cal_file",
                detail: "must be a non-empty trimmed name/path <=256 chars without control chars"
                    .to_string(),
            });
        }
        if !is_nonzero_sha256_hex(&self.cal_sha256) {
            return Err(CaptureError::InvalidConfig {
                field: "mic.cal_sha256",
                detail: "must be 64 lowercase hex chars and non-zero".to_string(),
            });
        }
        if self.orientation.trim().is_empty() {
            return Err(CaptureError::InvalidConfig {
                field: "mic.orientation",
                detail: "must declare 90deg/0deg (ACCEPTANCE 0.1)".to_string(),
            });
        }
        if self.clock_domain != UMIK_CLOCK_DOMAIN_USB {
            return Err(CaptureError::InvalidConfig {
                field: "mic.clock_domain",
                detail: format!(
                    "P1 UMIK must be an independent usb ADC (got {:?})",
                    self.clock_domain
                ),
            });
        }
        if self.channels != UMIK_REQUIRED_CHANNELS {
            return Err(CaptureError::UnexpectedChannelCount {
                role: CaptureStreamRole::UmikAdc.as_str(),
                expected: UMIK_REQUIRED_CHANNELS,
                actual: self.channels,
            });
        }
        let is_umik1 = self.model.eq_ignore_ascii_case("UMIK-1");
        if is_umik1 {
            if self.sample_rate_hz != UMIK1_NATIVE_SAMPLE_RATE_HZ {
                return Err(CaptureError::UnsupportedSampleRate {
                    actual: self.sample_rate_hz,
                    detail: "UMIK-1 supports only 48000 Hz (96k needs a different mic)".to_string(),
                });
            }
        } else if !SAMPLE_RATES_HZ.contains(&self.sample_rate_hz) {
            return Err(CaptureError::UnsupportedSampleRate {
                actual: self.sample_rate_hz,
                detail: format!("runtime accepts only {SAMPLE_RATES_HZ:?}"),
            });
        }
        Ok(())
    }

    /// 转成 T48 ingest MicInfo（报告/消费方共用；T48 schema 不含 cal_file）。
    pub fn mic_info(&self) -> MicInfo {
        MicInfo {
            model: self.model.clone(),
            serial: self.serial.clone(),
            cal_sha256: self.cal_sha256.clone(),
            orientation: self.orientation.clone(),
            clock_domain: self.clock_domain.clone(),
        }
    }

    /// 报告用麦克风声明（MicInfo + cal_file；ACCEPTANCE §0.1.2 要求报告记录 cal_file）。
    pub fn mic_report(&self) -> MicReport {
        MicReport {
            info: self.mic_info(),
            cal_file: self.cal_file.clone(),
        }
    }

    /// 是否按 90 度校准/麦轴垂直朝天花板（多声道听音位口径）。
    pub fn is_vertical_orientation(&self) -> bool {
        self.orientation == UMIK_ORIENTATION_VERTICAL
    }
}

/// 报告用麦克风声明（ACCEPTANCE §0.1.2：报告记录 mic{model, serial, cal_file, sha256, orientation}）。
///
/// T48 ingest 的 MicInfo schema 不含 cal_file，这里组合而不改消费者 schema；
/// `info.cal_sha256` 就是 cal_file 字节的 sha256（同一份文件，不重复造哈希字段）。
#[derive(Debug, Clone, PartialEq)]
pub struct MicReport {
    /// T48 ingest MicInfo（model/serial/cal_sha256/orientation/clock_domain）。
    pub info: MicInfo,
    /// 校准文件名/相对路径（必填；与 info.cal_sha256 指向同一份文件）。
    pub cal_file: String,
}

/// UMIK 采集会话配置（一次 T2/T4/T6 采集）。
#[derive(Debug, Clone, PartialEq)]
pub struct UmikCaptureConfig {
    /// UMIK 设备声明。
    pub device: UmikDeviceConfig,
    /// 会话 id（必须与 T61 timeline / T48 session 一致）。
    pub session_id: String,
    /// host epoch id（>0；公共 render frame 基准）。
    pub epoch_id: u64,
    /// host graph id（64 lowercase hex、非全零）。
    pub graph_id: String,
    /// render frame 0 对应的 mono 时刻（ns；>0 时用于从 block 时间戳推 seed）。
    pub epoch_start_mono_ns: u64,
    /// 采集块帧数（ALSA actual period / 读块大小；不要求等于 render block）。
    pub chunk_frames: u32,
    /// 可选显式 seed：render frame 0 对应的 capture sample 索引
    /// （采集先起（有 settle/预滚）=> 正值；采集晚于 render 0 => 负值）。
    /// 与 `AudioCaptureResult::seed_render_frame` 同义，`seed_from_stream` 直接返回。
    pub capture_start_render_frame: Option<f64>,
    /// 期望总帧数（可选；用于提前拒绝明显不完整的采集）。
    pub expected_total_frames: Option<u64>,
}

impl UmikCaptureConfig {
    /// 构造默认声学采集配置（epoch 1、T61 占位 graph id、128 帧块）。
    pub fn new(session_id: impl Into<String>, device: UmikDeviceConfig) -> Self {
        UmikCaptureConfig {
            device,
            session_id: session_id.into(),
            epoch_id: 1,
            graph_id: PLACEHOLDER_GRAPH_ID.to_string(),
            epoch_start_mono_ns: 0,
            chunk_frames: 128,
            capture_start_render_frame: None,
            expected_total_frames: None,
        }
    }

    /// 严格校验。
    pub fn validate(&self) -> Result<(), CaptureError> {
        self.device.validate()?;
        if self.session_id.trim().is_empty() || self.session_id.chars().count() > 128 {
            return Err(CaptureError::InvalidConfig {
                field: "session_id",
                detail: "must be non-empty and <=128 chars".to_string(),
            });
        }
        if self.epoch_id == 0 {
            return Err(CaptureError::InvalidConfig {
                field: "epoch_id",
                detail: "must be >0".to_string(),
            });
        }
        if !is_nonzero_graph_id(&self.graph_id) {
            return Err(CaptureError::InvalidConfig {
                field: "graph_id",
                detail: "must be 64 lowercase hex chars and non-zero".to_string(),
            });
        }
        if !(MIN_CHUNK_FRAMES..=MAX_CHUNK_FRAMES).contains(&self.chunk_frames) {
            return Err(CaptureError::InvalidConfig {
                field: "chunk_frames",
                detail: format!("must be in {MIN_CHUNK_FRAMES}..={MAX_CHUNK_FRAMES}"),
            });
        }
        if let Some(seed) = self.capture_start_render_frame {
            if !seed.is_finite() {
                return Err(CaptureError::InvalidConfig {
                    field: "capture_start_render_frame",
                    detail: "must be finite".to_string(),
                });
            }
        }
        if let Some(frames) = self.expected_total_frames {
            if frames == 0 {
                return Err(CaptureError::InvalidConfig {
                    field: "expected_total_frames",
                    detail: "must be >0".to_string(),
                });
            }
        }
        Ok(())
    }

    /// 采集总帧数上限（MAX_CAPTURE_SECONDS x fs）。
    pub fn max_frames(&self) -> u64 {
        (MAX_CAPTURE_SECONDS * self.device.sample_rate_hz as f64).floor() as u64
    }
}

/// MOTU 电学 reference 通道声明（WBS T62：MOTU 可选电学 reference 另 channel）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElectricalReferenceChannel {
    /// MOTU（双工）设备串；必须与 UMIK 设备不同。
    pub device: String,
    /// 选中的 MOTU 输入通道（0-based；< hw.n_in）。
    pub input_channel: u16,
    /// 该通道样本格式（必须与 hw.sample_format 一致）。
    pub sample_format: SampleFormat,
}

impl ElectricalReferenceChannel {
    /// 构造。
    pub fn new(device: impl Into<String>, input_channel: u16, sample_format: SampleFormat) -> Self {
        ElectricalReferenceChannel {
            device: device.into(),
            input_channel,
            sample_format,
        }
    }

    /// 校验（与具体 FormatConfig 无关的部分）。
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.device.trim().is_empty() || self.device.chars().count() > 128 {
            return Err(CaptureError::InvalidConfig {
                field: "reference.device",
                detail: "must be non-empty and <=128 chars".to_string(),
            });
        }
        Ok(())
    }
}

/// 映射 T22 hw.sample_format 到 T48 ingest SampleFormat（S16 不在 P1 校准采集口径内）。
fn ingest_sample_format_of(hw_format: u16) -> Result<SampleFormat, CaptureError> {
    match hw_format {
        sample_format::S24_3LE_INTERLEAVED => Ok(SampleFormat::S24_3LE),
        sample_format::S32_LE_INTERLEAVED => Ok(SampleFormat::S32LE),
        sample_format::F32_INTERLEAVED => Ok(SampleFormat::F32LE),
        other => Err(CaptureError::UnsupportedSampleFormat {
            role: CaptureStreamRole::MotuElectrical.as_str(),
            format: format!("hw_sample_format={other}"),
        }),
    }
}

/// MOTU 电学 reference 采集配置（双工设备；T22 FormatConfig 为实际 hwparams）。
#[derive(Debug, Clone, PartialEq)]
pub struct ElectricalReferenceConfig {
    /// reference 通道声明。
    pub channel: ElectricalReferenceChannel,
    /// T22 实际 hwparams（必须已由 T17/T26 协商并校验）。
    pub format: FormatConfig,
    /// 会话 id（必须与 UMIK/timeline 一致）。
    pub session_id: String,
    /// host epoch id（>0）。
    pub epoch_id: u64,
    /// host graph id（64 lowercase hex、非全零）。
    pub graph_id: String,
    /// render frame 0 对应 mono ns。
    pub epoch_start_mono_ns: u64,
    /// 可选显式 seed（同 UMIK 口径）。
    pub capture_start_render_frame: Option<f64>,
    /// 期望总帧数（可选）。
    pub expected_total_frames: Option<u64>,
    layout: PeriodLayout,
}

impl ElectricalReferenceConfig {
    /// 由通道声明 + T22 FormatConfig 构造；格式/通道/采样率在此一次性校验。
    pub fn new(
        channel: ElectricalReferenceChannel,
        format: FormatConfig,
    ) -> Result<Self, CaptureError> {
        channel.validate()?;
        let layout = format::validate(&format).map_err(|status| CaptureError::InvalidConfig {
            field: "reference.format",
            detail: format!("T22 FormatConfig rejected: {status:?}"),
        })?;
        let mapped = ingest_sample_format_of(format.hw.sample_format)?;
        if mapped != channel.sample_format {
            return Err(CaptureError::InvalidConfig {
                field: "reference.sample_format",
                detail: format!(
                    "channel.sample_format {:?} != hw.sample_format {:?}",
                    channel.sample_format, format.hw.sample_format
                ),
            });
        }
        if channel.input_channel as usize >= layout.n_in {
            return Err(CaptureError::InvalidConfig {
                field: "reference.input_channel",
                detail: format!(
                    "input_channel {} out of range (hw.n_in={})",
                    channel.input_channel, layout.n_in
                ),
            });
        }
        Ok(ElectricalReferenceConfig {
            channel,
            format,
            session_id: String::new(),
            epoch_id: 1,
            graph_id: PLACEHOLDER_GRAPH_ID.to_string(),
            epoch_start_mono_ns: 0,
            capture_start_render_frame: None,
            expected_total_frames: None,
            layout,
        })
    }

    /// T22 校验后的 period 布局。
    pub const fn layout(&self) -> &PeriodLayout {
        &self.layout
    }

    /// 采样率（来自 actual hwparams）。
    pub const fn sample_rate_hz(&self) -> u32 {
        self.format.hw.sample_rate
    }

    /// 采集块帧数（= actual period；T22 要求 64/128）。
    pub const fn chunk_frames(&self) -> u32 {
        self.layout.period_frames as u32
    }

    /// 每 block 的 dense interleaved 字节数（period x n_in x bytes_per_sample）。
    pub const fn block_bytes(&self) -> usize {
        self.layout.in_min_bytes
    }

    /// 严格校验（会话字段；格式/通道范围已由 new 保证）。
    pub fn validate(&self) -> Result<(), CaptureError> {
        self.channel.validate()?;
        if self.session_id.trim().is_empty() || self.session_id.chars().count() > 128 {
            return Err(CaptureError::InvalidConfig {
                field: "session_id",
                detail: "must be non-empty and <=128 chars".to_string(),
            });
        }
        if self.epoch_id == 0 {
            return Err(CaptureError::InvalidConfig {
                field: "epoch_id",
                detail: "must be >0".to_string(),
            });
        }
        if !is_nonzero_graph_id(&self.graph_id) {
            return Err(CaptureError::InvalidConfig {
                field: "graph_id",
                detail: "must be 64 lowercase hex chars and non-zero".to_string(),
            });
        }
        if !BLOCK_SIZES.contains(&(self.layout.period_frames as u16)) {
            return Err(CaptureError::InvalidConfig {
                field: "reference.format.period_frames",
                detail: format!("must be one of {BLOCK_SIZES:?}"),
            });
        }
        if let Some(seed) = self.capture_start_render_frame {
            if !seed.is_finite() {
                return Err(CaptureError::InvalidConfig {
                    field: "capture_start_render_frame",
                    detail: "must be finite".to_string(),
                });
            }
        }
        Ok(())
    }

    /// 采集总帧数上限。
    pub fn max_frames(&self) -> u64 {
        (MAX_CAPTURE_SECONDS * self.sample_rate_hz() as f64).floor() as u64
    }
}

// ---------------------------------------------------------------------------
// 设备分离计划（WBS T62 硬规则）
// ---------------------------------------------------------------------------

/// 采集/reference 运行计划：显式声明 drift source 与 common phase reference 的设备域。
///
/// 这是 WBS T62 漂移与公共相位基准不混设备的机器可检查形式：plan 一旦构造成功，
/// 分析入口还会再校验 CapturedStream 的设备与 plan 一致。
#[derive(Debug, Clone, PartialEq)]
pub struct ReferencePlan {
    /// 会话用途。
    pub purpose: CapturePurpose,
    /// UMIK 设备串（声学会话必填；电学环回必须 None）。
    pub umik_device: Option<String>,
    /// MOTU 电学 reference 通道（电学环回必填；声学可选 = auxiliary）。
    pub electrical: Option<ElectricalReferenceChannel>,
    /// 漂移估计设备域。
    pub drift_source: TimebaseDriftSource,
    /// 公共相位基准设备域。
    pub phase_reference: CommonPhaseReference,
}

impl ReferencePlan {
    /// 声学会话计划：drift 与相位基准都在 UMIK；MOTU 电学 reference 仅 auxiliary。
    pub fn acoustic(
        device: &UmikDeviceConfig,
        electrical: Option<ElectricalReferenceChannel>,
    ) -> Result<Self, CaptureError> {
        let plan = ReferencePlan {
            purpose: CapturePurpose::AcousticRoom,
            umik_device: Some(device.device.clone()),
            electrical,
            drift_source: TimebaseDriftSource::UmikRepeatGrid,
            phase_reference: CommonPhaseReference::UmikDirectOnset,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// 电学环回计划：drift 与相位基准都在 MOTU 电学 reference（UMIK 不参与）。
    pub fn electrical_loopback(
        electrical: ElectricalReferenceChannel,
    ) -> Result<Self, CaptureError> {
        let plan = ReferencePlan {
            purpose: CapturePurpose::ElectricalLoopback,
            umik_device: None,
            electrical: Some(electrical),
            drift_source: TimebaseDriftSource::MotuElectricalLoopback,
            phase_reference: CommonPhaseReference::MotuElectricalLoopback,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// 主采集流角色（drift 与相位基准同域；校验后二者相等）。
    pub const fn primary_role(&self) -> CaptureStreamRole {
        self.drift_source.role()
    }

    /// 严格校验不混设备硬规则。
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.drift_source.role() != self.phase_reference.role() {
            return Err(CaptureError::MixedReferenceDevices {
                detail: format!(
                    "drift_source {} and phase_reference {} are in different device domains",
                    self.drift_source.as_str(),
                    self.phase_reference.as_str()
                ),
            });
        }
        match self.purpose {
            CapturePurpose::AcousticRoom => {
                if self.drift_source != TimebaseDriftSource::UmikRepeatGrid {
                    return Err(CaptureError::MixedReferenceDevices {
                        detail:
                            "acoustic capture drift must be estimated from the UMIK repeat grid"
                                .to_string(),
                    });
                }
                if self.phase_reference != CommonPhaseReference::UmikDirectOnset {
                    return Err(CaptureError::MixedReferenceDevices {
                        detail: "acoustic common phase reference must be the UMIK direct onset"
                            .to_string(),
                    });
                }
                let umik = self.umik_device.as_deref().unwrap_or_default();
                if umik.trim().is_empty() {
                    return Err(CaptureError::InvalidConfig {
                        field: "plan.umik_device",
                        detail: "acoustic capture requires a UMIK device".to_string(),
                    });
                }
                if let Some(electrical) = &self.electrical {
                    if electrical.device == umik {
                        return Err(CaptureError::ReferenceNotIndependent {
                            device: electrical.device.clone(),
                        });
                    }
                }
            }
            CapturePurpose::ElectricalLoopback => {
                if self.drift_source != TimebaseDriftSource::MotuElectricalLoopback {
                    return Err(CaptureError::MixedReferenceDevices {
                        detail:
                            "electrical loopback drift must come from the MOTU electrical reference"
                                .to_string(),
                    });
                }
                if self.phase_reference != CommonPhaseReference::MotuElectricalLoopback {
                    return Err(CaptureError::MixedReferenceDevices {
                        detail:
                            "electrical loopback phase reference must be the MOTU electrical channel"
                                .to_string(),
                    });
                }
                let electrical =
                    self.electrical
                        .as_ref()
                        .ok_or_else(|| CaptureError::InvalidConfig {
                            field: "plan.electrical",
                            detail: "electrical loopback requires an electrical reference channel"
                                .to_string(),
                        })?;
                if let Some(umik) = &self.umik_device {
                    if umik == &electrical.device {
                        return Err(CaptureError::ReferenceNotIndependent {
                            device: electrical.device.clone(),
                        });
                    }
                    return Err(CaptureError::MixedReferenceDevices {
                        detail: "electrical loopback must not reference the UMIK acoustic ADC"
                            .to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// T61 时间轴 -> repeat grid
// ---------------------------------------------------------------------------

/// 一个会话槽位的 repeat 时间轴（render frame 域）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatSlot {
    /// layout 容器槽位（SweepOutput.slot）。
    pub container_slot: u32,
    /// 角色 id。
    pub speaker: String,
    /// 播放设备通道号。
    pub channel: u32,
    /// 本槽首遍扫频起点 render frame（>0）。
    pub render_frame_index: u64,
    /// 每遍扫频起点 render frame（长度 = repeats；严格递增）。
    pub repeat_render_frames: Vec<u64>,
}

/// 从 SweepTimeline 提取的 repeat 网格（T62 时基估计输入）。
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatGrid {
    /// session id。
    pub session_id: String,
    /// layout id。
    pub layout: String,
    /// host epoch id（>0）。
    pub epoch_id: u64,
    /// host graph id。
    pub graph_id: String,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// render block 帧数（64/128）。
    pub block_frames: u16,
    /// 扫频起始频率（Hz）。
    pub start_hz: f64,
    /// 扫频结束频率（Hz）。
    pub end_hz: f64,
    /// 单遍扫频帧数（block 对齐）。
    pub sweep_frames: u64,
    /// 遍间静音帧数（block 对齐）。
    pub gap_frames: u64,
    /// fade 帧数（单侧）。
    pub fade_frames: u64,
    /// 前静音帧数。
    pub pre_frames: u64,
    /// 后静音帧数。
    pub post_frames: u64,
    /// 会话总帧数（timeline 计划）。
    pub total_frames: u64,
    /// 驱动电平（dBFS）。
    pub level_dbfs: f64,
    /// 驱动峰值线性幅度（= 10^(effective/20)）。
    pub peak_linear: f64,
    /// 每槽重复次数（1..=10）。
    pub repeats: u32,
    /// 逐槽 repeat 时间轴（顺序 = 播放顺序）。
    pub slots: Vec<RepeatSlot>,
}

impl RepeatGrid {
    /// 从 T61 SweepTimeline 提取并校验 repeat 网格。
    pub fn from_timeline(timeline: &SweepTimeline) -> Result<Self, CaptureError> {
        if timeline.session_id.trim().is_empty() {
            return Err(CaptureError::InvalidTimeline {
                detail: "timeline session_id is empty".to_string(),
            });
        }
        if timeline.epoch_id == 0 {
            return Err(CaptureError::InvalidTimeline {
                detail: "timeline epoch_id must be >0".to_string(),
            });
        }
        if !is_nonzero_graph_id(&timeline.graph_id) {
            return Err(CaptureError::InvalidTimeline {
                detail: "timeline graph_id must be 64 lowercase hex and non-zero".to_string(),
            });
        }
        if !SAMPLE_RATES_HZ.contains(&timeline.sample_rate) {
            return Err(CaptureError::UnsupportedSampleRate {
                actual: timeline.sample_rate,
                detail: format!("timeline accepts only {SAMPLE_RATES_HZ:?}"),
            });
        }
        if !BLOCK_SIZES.contains(&timeline.block_frames) {
            return Err(CaptureError::InvalidTimeline {
                detail: format!("timeline block_frames must be one of {BLOCK_SIZES:?}"),
            });
        }
        let repeats = timeline.stimulus.repeats;
        if !(1..=MAX_REPEATS_PER_SLOT).contains(&repeats) {
            return Err(CaptureError::InvalidTimeline {
                detail: format!("stimulus.repeats must be in 1..={MAX_REPEATS_PER_SLOT}"),
            });
        }
        if timeline.slots.is_empty() || timeline.slots.len() > MAX_REPEAT_SLOTS {
            return Err(CaptureError::InvalidTimeline {
                detail: format!("timeline needs 1..={MAX_REPEAT_SLOTS} slots"),
            });
        }
        if timeline.actual_stimulus.sweep_frames == 0 {
            return Err(CaptureError::InvalidTimeline {
                detail: "actual sweep frames must be >0".to_string(),
            });
        }
        if !finite_positive(timeline.stimulus.start_hz)
            || !finite_positive(timeline.stimulus.end_hz)
            || timeline.stimulus.end_hz <= timeline.stimulus.start_hz
        {
            return Err(CaptureError::InvalidTimeline {
                detail: "stimulus start/end hz must be finite and increasing".to_string(),
            });
        }
        if !finite_positive(timeline.level.peak_linear) {
            return Err(CaptureError::InvalidTimeline {
                detail: "timeline level.peak_linear must be finite and >0".to_string(),
            });
        }

        let mut slots = Vec::with_capacity(timeline.slots.len());
        let mut previous_slot_start = 0u64;
        for slot in &timeline.slots {
            if slot.repeat_render_frames.len() != repeats as usize {
                return Err(CaptureError::InvalidTimeline {
                    detail: format!(
                        "slot {} ({}) has {} repeat frames, expected {repeats}",
                        slot.slot,
                        slot.speaker,
                        slot.repeat_render_frames.len()
                    ),
                });
            }
            if slot.render_frame_index != slot.repeat_render_frames[0] {
                return Err(CaptureError::InvalidTimeline {
                    detail: format!(
                        "slot {} render_frame_index != first repeat frame",
                        slot.slot
                    ),
                });
            }
            let mut previous = 0u64;
            for (index, frame) in slot.repeat_render_frames.iter().enumerate() {
                if *frame == 0 {
                    return Err(CaptureError::InvalidTimeline {
                        detail: format!(
                            "slot {} repeat {index} render frame must be >0",
                            slot.slot
                        ),
                    });
                }
                if index > 0 && *frame <= previous {
                    return Err(CaptureError::InvalidTimeline {
                        detail: format!(
                            "slot {} repeat frames must be strictly increasing",
                            slot.slot
                        ),
                    });
                }
                previous = *frame;
            }
            if slot.render_frame_index <= previous_slot_start {
                return Err(CaptureError::InvalidTimeline {
                    detail: format!("slot {} start is not after the previous slot", slot.slot),
                });
            }
            previous_slot_start = slot.render_frame_index;
            let last_repeat = *slot.repeat_render_frames.last().unwrap_or(&0);
            let sweep_end = last_repeat.saturating_add(timeline.actual_stimulus.sweep_frames);
            if sweep_end > timeline.total_frames {
                return Err(CaptureError::InvalidTimeline {
                    detail: format!(
                        "slot {} sweep ends at frame {sweep_end} beyond timeline total {}",
                        slot.slot, timeline.total_frames
                    ),
                });
            }
            slots.push(RepeatSlot {
                container_slot: slot.slot,
                speaker: slot.speaker.clone(),
                channel: slot.channel,
                render_frame_index: slot.render_frame_index,
                repeat_render_frames: slot.repeat_render_frames.clone(),
            });
        }

        Ok(RepeatGrid {
            session_id: timeline.session_id.clone(),
            layout: timeline.layout.clone(),
            epoch_id: timeline.epoch_id,
            graph_id: timeline.graph_id.clone(),
            sample_rate_hz: timeline.sample_rate,
            block_frames: timeline.block_frames,
            start_hz: timeline.stimulus.start_hz,
            end_hz: timeline.stimulus.end_hz,
            sweep_frames: timeline.actual_stimulus.sweep_frames,
            gap_frames: timeline.actual_stimulus.gap_frames,
            fade_frames: timeline.actual_stimulus.fade_frames,
            pre_frames: timeline.pre_frames,
            post_frames: timeline.post_frames,
            total_frames: timeline.total_frames,
            level_dbfs: timeline.level.drive_level_dbfs,
            peak_linear: timeline.level.peak_linear,
            repeats,
            slots,
        })
    }

    /// 有效 repeat 起点总数（slot x repeats）。
    pub fn repeat_count(&self) -> usize {
        self.slots.len() * self.repeats as usize
    }

    /// 首遍扫频起点 render frame。
    pub fn first_render_frame(&self) -> Option<u64> {
        self.slots.first().map(|slot| slot.render_frame_index)
    }
}

// ---------------------------------------------------------------------------
// onset 检测配置
// ---------------------------------------------------------------------------

/// onset 检测参数（报告必须携带实际值）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnsetConfig {
    /// 搜索半窗（秒，向后 = expected - half）。
    pub search_half_window_seconds: f64,
    /// 前向搜索窗（秒，expected + forward）；20 Hz 起扫需覆盖 LF 滚降段。
    pub search_forward_seconds: f64,
    /// 阈值 = 带限 LF 参考包络峰值 x fraction（与噪声底门取 max）。
    pub threshold_fraction: f64,
    /// 噪声底门（dB）：阈值还须 >= LF 包络稳健噪声底 + 该值（双门，避免被噪声触发）。
    pub noise_gate_db: f64,
    /// 相邻 repeat 间隔容差（比例）。
    pub interval_tolerance_fraction: f64,
    /// 相邻 repeat 间隔容差（绝对样本下限）。
    pub interval_tolerance_samples: f64,
}

impl Default for OnsetConfig {
    fn default() -> Self {
        OnsetConfig {
            search_half_window_seconds: DEFAULT_ONSET_SEARCH_HALF_WINDOW_SECONDS,
            search_forward_seconds: DEFAULT_ONSET_SEARCH_FORWARD_SECONDS,
            threshold_fraction: DEFAULT_ONSET_THRESHOLD_FRACTION,
            noise_gate_db: DEFAULT_ONSET_NOISE_GATE_DB,
            interval_tolerance_fraction: ONSET_INTERVAL_TOLERANCE_FRACTION,
            interval_tolerance_samples: ONSET_INTERVAL_TOLERANCE_SAMPLES,
        }
    }
}

impl OnsetConfig {
    /// 严格校验。
    pub fn validate(&self) -> Result<(), CaptureError> {
        if !self.search_half_window_seconds.is_finite()
            || !(MIN_ONSET_SEARCH_HALF_WINDOW_SECONDS..=MAX_ONSET_SEARCH_HALF_WINDOW_SECONDS)
                .contains(&self.search_half_window_seconds)
        {
            return Err(CaptureError::InvalidConfig {
                field: "onset.search_half_window_seconds",
                detail: format!(
                    "must be in {MIN_ONSET_SEARCH_HALF_WINDOW_SECONDS}..={MAX_ONSET_SEARCH_HALF_WINDOW_SECONDS}"
                ),
            });
        }
        if !self.search_forward_seconds.is_finite()
            || !(MIN_ONSET_SEARCH_FORWARD_SECONDS..=MAX_ONSET_SEARCH_FORWARD_SECONDS)
                .contains(&self.search_forward_seconds)
        {
            return Err(CaptureError::InvalidConfig {
                field: "onset.search_forward_seconds",
                detail: format!(
                    "must be in {MIN_ONSET_SEARCH_FORWARD_SECONDS}..={MAX_ONSET_SEARCH_FORWARD_SECONDS}"
                ),
            });
        }
        if !self.threshold_fraction.is_finite()
            || !(MIN_ONSET_THRESHOLD_FRACTION..=MAX_ONSET_THRESHOLD_FRACTION)
                .contains(&self.threshold_fraction)
        {
            return Err(CaptureError::InvalidConfig {
                field: "onset.threshold_fraction",
                detail: format!(
                    "must be in {MIN_ONSET_THRESHOLD_FRACTION}..={MAX_ONSET_THRESHOLD_FRACTION}"
                ),
            });
        }
        if !self.noise_gate_db.is_finite()
            || !(MIN_ONSET_NOISE_GATE_DB..=MAX_ONSET_NOISE_GATE_DB).contains(&self.noise_gate_db)
        {
            return Err(CaptureError::InvalidConfig {
                field: "onset.noise_gate_db",
                detail: format!("must be in {MIN_ONSET_NOISE_GATE_DB}..={MAX_ONSET_NOISE_GATE_DB}"),
            });
        }
        if !finite_positive(self.interval_tolerance_fraction)
            || !finite_positive(self.interval_tolerance_samples)
        {
            return Err(CaptureError::InvalidConfig {
                field: "onset.interval_tolerance",
                detail: "must be finite and >0".to_string(),
            });
        }
        Ok(())
    }

    /// 某期望间隔（render frames）的容差（样本）。
    pub fn interval_tolerance(self, interval_frames: u64) -> f64 {
        (self.interval_tolerance_fraction * interval_frames as f64)
            .max(self.interval_tolerance_samples)
    }
}

// ---------------------------------------------------------------------------
// 采集数据（raw + block 日志 + 汇总）
// ---------------------------------------------------------------------------

/// 一个采集 block 的日志（时间戳/样本区间/质量计数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureBlockLog {
    /// block 序号（0-based）。
    pub index: u64,
    /// block 首样本的 mono ns（主机时钟）。
    pub mono_ns: u64,
    /// block 首样本的 capture 样本下标。
    pub start_sample: u64,
    /// block 帧数。
    pub frames: u32,
    /// 本 block 数字削波样本数。
    pub clipped: u64,
    /// 本 block nonfinite 样本数。
    pub nonfinite: u64,
}

/// 采集流汇总（报告/JSON 用；不含原始样本）。
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureStreamSummary {
    /// 角色。
    pub role: CaptureStreamRole,
    /// 设备串。
    pub device: String,
    /// session id。
    pub session_id: String,
    /// epoch id。
    pub epoch_id: u64,
    /// graph id。
    pub graph_id: String,
    /// 样本格式。
    pub sample_format: SampleFormat,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// 通道数（raw 产物固定 1ch dense）。
    pub channels: u16,
    /// 读块帧数。
    pub chunk_frames: u32,
    /// 总帧数。
    pub frames: u64,
    /// 总时长（秒）。
    pub seconds: f64,
    /// block 数。
    pub blocks: u64,
    /// 采集峰值（dBFS；无信号为 SILENCE_DBFS）。
    pub capture_peak_dbfs: f64,
    /// 数字削波样本数。
    pub clipped_samples: u64,
    /// nonfinite 样本数（已 sanitize）。
    pub nonfinite_samples: u64,
    /// 峰值门（ACCEPTANCE §0.5；<= -1 dBFS 且无削波）。
    pub peak_ceiling_dbfs: f64,
    /// 峰值门是否通过。
    pub peak_gate_pass: bool,
    /// 前静音噪声底（dBFS；无效为 None）。
    pub noise_floor_dbfs: Option<f64>,
    /// 原始字节 sha256（小写 hex）。
    pub raw_sha256: String,
    /// UMIK mic 声明（仅 UmikAdc；含报告要求的 cal_file）。
    pub mic: Option<MicReport>,
    /// MOTU 电学 reference 输入通道（仅 MotuElectrical）。
    pub reference_channel: Option<u16>,
}

/// 一个已经采集完成的流（raw + 解码后的单通道样本 + block 日志）。
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedStream {
    /// 角色。
    pub role: CaptureStreamRole,
    /// 设备串。
    pub device: String,
    /// session id。
    pub session_id: String,
    /// host epoch id。
    pub epoch_id: u64,
    /// host graph id。
    pub graph_id: String,
    /// render frame 0 的 mono ns（0 = 未提供）。
    pub epoch_start_mono_ns: u64,
    /// 显式 seed：render frame 0 对应的 capture sample 索引（采集先起 => 正值）。
    pub capture_start_render_frame: Option<f64>,
    /// 样本格式。
    pub sample_format: SampleFormat,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// 读块帧数。
    pub chunk_frames: u32,
    /// raw 字节（1ch dense，设备格式）。
    pub raw: Vec<u8>,
    /// 解码后的单通道样本（sanitized/clamped）。
    pub samples: Vec<f32>,
    /// block 日志。
    pub blocks: Vec<CaptureBlockLog>,
    /// 峰值 |x|（sanitize 后）。
    pub peak_abs: f64,
    /// 数字削波样本数。
    pub clipped_samples: u64,
    /// nonfinite 样本数。
    pub nonfinite_samples: u64,
    /// 总帧数。
    pub frames: u64,
    /// MOTU 电学 reference 输入通道（仅 MotuElectrical）。
    pub reference_channel: Option<u16>,
}

impl CapturedStream {
    /// 总时长（秒）。
    pub fn seconds(&self) -> f64 {
        if self.sample_rate_hz == 0 {
            0.0
        } else {
            self.frames as f64 / self.sample_rate_hz as f64
        }
    }

    /// 采集峰值（dBFS）。
    pub fn capture_peak_dbfs(&self) -> f64 {
        dbfs_of(self.peak_abs)
    }

    /// 生成 T48 ingest CaptureFile（sha256 绑定 raw 字节；不写文件系统）。
    pub fn capture_file(&self, relative_path: &str) -> Result<CaptureFile, CaptureError> {
        validate_relative_artifact_path(relative_path)?;
        Ok(CaptureFile {
            relative_path: relative_path.to_string(),
            sha256: sha256_hex(&self.raw),
            bytes: self.raw.len() as u64,
            frames: self.frames,
            channels: 1,
            sample_rate_hz: self.sample_rate_hz,
            sample_format: self.sample_format,
        })
    }

    /// 采集流汇总（不含原始样本）。
    pub fn summary(&self) -> CaptureStreamSummary {
        let peak_ceiling_linear = 10.0f64.powf(CAPTURE_PEAK_CEILING_DBFS / 20.0);
        CaptureStreamSummary {
            role: self.role,
            device: self.device.clone(),
            session_id: self.session_id.clone(),
            epoch_id: self.epoch_id,
            graph_id: self.graph_id.clone(),
            sample_format: self.sample_format,
            sample_rate_hz: self.sample_rate_hz,
            channels: 1,
            chunk_frames: self.chunk_frames,
            frames: self.frames,
            seconds: self.seconds(),
            blocks: self.blocks.len() as u64,
            capture_peak_dbfs: self.capture_peak_dbfs(),
            clipped_samples: self.clipped_samples,
            nonfinite_samples: self.nonfinite_samples,
            peak_ceiling_dbfs: CAPTURE_PEAK_CEILING_DBFS,
            peak_gate_pass: self.peak_abs <= peak_ceiling_linear
                && self.clipped_samples == 0
                && self.peak_abs > 0.0,
            noise_floor_dbfs: None,
            raw_sha256: sha256_hex(&self.raw),
            mic: None,
            reference_channel: self.reference_channel,
        }
    }
}

fn validate_relative_artifact_path(path: &str) -> Result<(), CaptureError> {
    if path.trim().is_empty() || path.chars().count() > 256 {
        return Err(CaptureError::InvalidConfig {
            field: "capture_file.relative_path",
            detail: "must be non-empty and <=256 chars".to_string(),
        });
    }
    if path.contains('\\') || path.contains("://") || path.starts_with('/') {
        return Err(CaptureError::InvalidConfig {
            field: "capture_file.relative_path",
            detail: "must be a relative '/'-separated path".to_string(),
        });
    }
    if path.split('/').any(|part| part == ".." || part.is_empty()) {
        return Err(CaptureError::InvalidConfig {
            field: "capture_file.relative_path",
            detail: "path must not contain '..' or empty segments".to_string(),
        });
    }
    Ok(())
}

fn sample_format_bytes(format: SampleFormat) -> usize {
    format.bytes_per_sample() as usize
}

/// 按 T22 语义解码单通道设备样本：nonfinite -> 0 并计数；|x|>1 clamp 并计数。
fn decode_mono_samples(
    format: SampleFormat,
    bytes: &[u8],
    out: &mut Vec<f32>,
) -> Result<(u64, u64), CaptureError> {
    let bps = sample_format_bytes(format);
    if bps == 0 || bytes.len() % bps != 0 {
        return Err(CaptureError::InvalidBlock {
            block: 0,
            detail: format!(
                "byte length {} is not a multiple of {} bytes/sample for {:?}",
                bytes.len(),
                bps,
                format
            ),
        });
    }
    let mut clipped = 0u64;
    let mut nonfinite = 0u64;
    out.reserve(bytes.len() / bps);
    for chunk in bytes.chunks_exact(bps) {
        let mut sample = match format {
            SampleFormat::S24_3LE => {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], 0]);
                let signed = (value << 8) >> 8;
                signed as f64 / 8_388_608.0
            }
            SampleFormat::S32LE => {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                value as f64 / 2_147_483_648.0
            }
            SampleFormat::F32LE => {
                f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f64
            }
        };
        if !sample.is_finite() {
            nonfinite = nonfinite.saturating_add(1);
            sample = 0.0;
        }
        if sample > 1.0 {
            sample = 1.0;
            clipped = clipped.saturating_add(1);
        } else if sample < -1.0 {
            sample = -1.0;
            clipped = clipped.saturating_add(1);
        }
        out.push(sample as f32);
    }
    Ok((clipped, nonfinite))
}

#[cfg(test)]
fn encode_mono_samples(format: SampleFormat, samples: &[f32], out: &mut Vec<u8>) {
    let bps = sample_format_bytes(format);
    out.reserve(samples.len() * bps);
    for sample in samples {
        match format {
            SampleFormat::S24_3LE => {
                let scaled = (*sample as f64 * 8_388_608.0).round();
                let value = if scaled >= 8_388_607.0 {
                    8_388_607
                } else if scaled <= -8_388_608.0 {
                    -8_388_608
                } else {
                    scaled as i32
                };
                let bytes = value.to_le_bytes();
                out.extend_from_slice(&bytes[0..3]);
            }
            SampleFormat::S32LE => {
                let scaled = (*sample as f64 * 2_147_483_648.0).round();
                let value = if scaled >= 2_147_483_647.0 {
                    2_147_483_647
                } else if scaled <= -2_147_483_648.0 {
                    -2_147_483_648
                } else {
                    scaled as i32
                };
                out.extend_from_slice(&value.to_le_bytes());
            }
            SampleFormat::F32LE => out.extend_from_slice(&sample.to_le_bytes()),
        }
    }
}

/// 采集构建器（两个角色共用；块日志/上限/时间戳纪律）。
#[derive(Debug, Clone)]
struct StreamBuilder {
    role: CaptureStreamRole,
    device: String,
    session_id: String,
    epoch_id: u64,
    graph_id: String,
    epoch_start_mono_ns: u64,
    capture_start_render_frame: Option<f64>,
    sample_format: SampleFormat,
    sample_rate_hz: u32,
    chunk_frames: u32,
    expected_total_frames: Option<u64>,
    max_frames: u64,
    raw: Vec<u8>,
    samples: Vec<f32>,
    blocks: Vec<CaptureBlockLog>,
    peak_abs: f64,
    clipped_samples: u64,
    nonfinite_samples: u64,
    next_sample: u64,
    last_mono_ns: Option<u64>,
    reference_channel: Option<u16>,
}

impl StreamBuilder {
    #[allow(clippy::too_many_arguments)]
    fn new(
        role: CaptureStreamRole,
        device: String,
        session_id: String,
        epoch_id: u64,
        graph_id: String,
        epoch_start_mono_ns: u64,
        capture_start_render_frame: Option<f64>,
        sample_format: SampleFormat,
        sample_rate_hz: u32,
        chunk_frames: u32,
        expected_total_frames: Option<u64>,
        max_frames: u64,
        reference_channel: Option<u16>,
    ) -> Self {
        StreamBuilder {
            role,
            device,
            session_id,
            epoch_id,
            graph_id,
            epoch_start_mono_ns,
            capture_start_render_frame,
            sample_format,
            sample_rate_hz,
            chunk_frames,
            expected_total_frames,
            max_frames,
            raw: Vec::new(),
            samples: Vec::new(),
            blocks: Vec::new(),
            peak_abs: 0.0,
            clipped_samples: 0,
            nonfinite_samples: 0,
            next_sample: 0,
            last_mono_ns: None,
            reference_channel,
        }
    }

    /// 登记一个 block（样本已解码/计数完成）。
    fn record_block(
        &mut self,
        mono_ns: u64,
        frames: u32,
        raw: &[u8],
        decoded: &[f32],
        clipped: u64,
        nonfinite: u64,
    ) -> Result<CaptureBlockLog, CaptureError> {
        if frames == 0 || frames > self.chunk_frames {
            return Err(CaptureError::InvalidBlock {
                block: self.blocks.len() as u64,
                detail: format!("frames {frames} outside 1..={}", self.chunk_frames),
            });
        }
        if decoded.len() != frames as usize {
            return Err(CaptureError::InvalidBlock {
                block: self.blocks.len() as u64,
                detail: format!("decoded {} samples for {frames} frames", decoded.len()),
            });
        }
        if let Some(previous) = self.last_mono_ns {
            if mono_ns <= previous {
                return Err(CaptureError::NonMonotonicStamp {
                    previous_ns: previous,
                    actual_ns: mono_ns,
                    block: self.blocks.len() as u64,
                });
            }
        }
        let requested = self.next_sample.saturating_add(frames as u64);
        if requested > self.max_frames {
            return Err(CaptureError::TooManyFrames {
                limit: self.max_frames,
                requested,
            });
        }
        if let Some(expected) = self.expected_total_frames {
            if requested > expected {
                return Err(CaptureError::TooManyFrames {
                    limit: expected,
                    requested,
                });
            }
        }
        self.raw.extend_from_slice(raw);
        for sample in decoded {
            let abs = (*sample as f64).abs();
            if abs > self.peak_abs {
                self.peak_abs = abs;
            }
            self.samples.push(*sample);
        }
        self.clipped_samples = self.clipped_samples.saturating_add(clipped);
        self.nonfinite_samples = self.nonfinite_samples.saturating_add(nonfinite);
        let log = CaptureBlockLog {
            index: self.blocks.len() as u64,
            mono_ns,
            start_sample: self.next_sample,
            frames,
            clipped,
            nonfinite,
        };
        self.blocks.push(log);
        self.next_sample = requested;
        self.last_mono_ns = Some(mono_ns);
        Ok(log)
    }

    fn finish(self) -> Result<CapturedStream, CaptureError> {
        if self.next_sample == 0 {
            return Err(CaptureError::InvalidBlock {
                block: 0,
                detail: "capture finished with zero frames".to_string(),
            });
        }
        Ok(CapturedStream {
            role: self.role,
            device: self.device,
            session_id: self.session_id,
            epoch_id: self.epoch_id,
            graph_id: self.graph_id,
            epoch_start_mono_ns: self.epoch_start_mono_ns,
            capture_start_render_frame: self.capture_start_render_frame,
            sample_format: self.sample_format,
            sample_rate_hz: self.sample_rate_hz,
            chunk_frames: self.chunk_frames,
            raw: self.raw,
            samples: self.samples,
            blocks: self.blocks,
            peak_abs: self.peak_abs,
            clipped_samples: self.clipped_samples,
            nonfinite_samples: self.nonfinite_samples,
            frames: self.next_sample,
            reference_channel: self.reference_channel,
        })
    }
}

// ---------------------------------------------------------------------------
// 采集对象：UMIK / MOTU 电学 reference
// ---------------------------------------------------------------------------

/// UMIK 独立 USB ADC 1ch 声学采集（离线可测；真实读块由 T26 接入）。
#[derive(Debug, Clone)]
pub struct UmikCapture {
    config: UmikCaptureConfig,
    builder: StreamBuilder,
}

impl UmikCapture {
    /// 构造（配置严格校验；不打开设备）。
    pub fn new(config: UmikCaptureConfig) -> Result<Self, CaptureError> {
        config.validate()?;
        let builder = StreamBuilder::new(
            CaptureStreamRole::UmikAdc,
            config.device.device.clone(),
            config.session_id.clone(),
            config.epoch_id,
            config.graph_id.clone(),
            config.epoch_start_mono_ns,
            config.capture_start_render_frame,
            config.device.sample_format,
            config.device.sample_rate_hz,
            config.chunk_frames,
            config.expected_total_frames,
            config.max_frames(),
            None,
        );
        Ok(UmikCapture { config, builder })
    }

    /// 会话配置。
    pub fn config(&self) -> &UmikCaptureConfig {
        &self.config
    }

    /// 已采集帧数。
    pub fn frames(&self) -> u64 {
        self.builder.next_sample
    }

    /// 推入一个 UMIK 读块（1ch dense 设备字节；块首样本 mono ns）。
    pub fn push_block(
        &mut self,
        mono_ns: u64,
        interleaved: &[u8],
    ) -> Result<CaptureBlockLog, CaptureError> {
        let bps = sample_format_bytes(self.config.device.sample_format);
        if interleaved.is_empty() || interleaved.len() % bps != 0 {
            return Err(CaptureError::InvalidBlock {
                block: self.builder.blocks.len() as u64,
                detail: format!(
                    "UMIK block bytes {} is not a positive multiple of {bps}",
                    interleaved.len()
                ),
            });
        }
        let frames = interleaved.len() / bps;
        let frames = u32::try_from(frames).map_err(|_| CaptureError::InvalidBlock {
            block: self.builder.blocks.len() as u64,
            detail: format!("block frames {frames} exceeds u32"),
        })?;
        let mut decoded = Vec::with_capacity(frames as usize);
        let (clipped, nonfinite) =
            decode_mono_samples(self.config.device.sample_format, interleaved, &mut decoded)?;
        self.builder
            .record_block(mono_ns, frames, interleaved, &decoded, clipped, nonfinite)
    }

    /// 结束采集（返回 raw/repeat/timebase 分析的输入流）。
    pub fn finish(self) -> Result<CapturedStream, CaptureError> {
        self.builder.finish()
    }
}

/// MOTU 电学 reference 采集（双工设备的一个输入通道；独立于 UMIK）。
#[derive(Debug, Clone)]
pub struct ElectricalReferenceCapture {
    config: ElectricalReferenceConfig,
    builder: StreamBuilder,
}

impl ElectricalReferenceCapture {
    /// 构造（T22 FormatConfig 严格校验；不打开设备）。
    pub fn new(config: ElectricalReferenceConfig) -> Result<Self, CaptureError> {
        config.validate()?;
        let builder = StreamBuilder::new(
            CaptureStreamRole::MotuElectrical,
            config.channel.device.clone(),
            config.session_id.clone(),
            config.epoch_id,
            config.graph_id.clone(),
            config.epoch_start_mono_ns,
            config.capture_start_render_frame,
            config.channel.sample_format,
            config.sample_rate_hz(),
            config.chunk_frames(),
            config.expected_total_frames,
            config.max_frames(),
            Some(config.channel.input_channel),
        );
        Ok(ElectricalReferenceCapture { config, builder })
    }

    /// reference 配置。
    pub fn config(&self) -> &ElectricalReferenceConfig {
        &self.config
    }

    /// 已采集帧数。
    pub fn frames(&self) -> u64 {
        self.builder.next_sample
    }

    /// 推入一个双工设备读块（dense interleaved period x n_in x bytes）。
    ///
    /// 走 T22 format::convert_capture 解码（nonfinite sanitize / |x|>1 clip 统计），
    /// 只保留声明的输入通道为 1ch；raw 产物也存该通道 dense 字节。
    pub fn push_block(
        &mut self,
        mono_ns: u64,
        interleaved: &[u8],
    ) -> Result<CaptureBlockLog, CaptureError> {
        let layout = *self.config.layout();
        let expected_bytes = self.config.block_bytes();
        if interleaved.len() != expected_bytes {
            return Err(CaptureError::InvalidBlock {
                block: self.builder.blocks.len() as u64,
                detail: format!(
                    "duplex block has {} bytes, expected {expected_bytes} (period {} x n_in {} x bps {})",
                    interleaved.len(),
                    layout.period_frames,
                    layout.n_in,
                    layout.bytes_per_sample
                ),
            });
        }
        let mut planar = vec![0.0f32; layout.period_frames * layout.n_in];
        let stats = format::convert_capture(&self.config.format, interleaved, &mut planar)
            .map_err(|status| CaptureError::InvalidBlock {
                block: self.builder.blocks.len() as u64,
                detail: format!("T22 convert_capture rejected: {status:?}"),
            })?;
        let channel = self.config.channel.input_channel as usize;
        let start = channel * layout.period_frames;
        let channel_samples = planar[start..start + layout.period_frames].to_vec();
        let raw_channel = extract_channel_bytes(
            interleaved,
            layout.n_in,
            layout.bytes_per_sample,
            channel,
            layout.period_frames,
        );
        let frames = layout.period_frames as u32;
        self.builder.record_block(
            mono_ns,
            frames,
            &raw_channel,
            &channel_samples,
            stats.clipped,
            stats.nonfinite_in,
        )
    }

    /// 结束采集。
    pub fn finish(self) -> Result<CapturedStream, CaptureError> {
        self.builder.finish()
    }
}

fn extract_channel_bytes(
    interleaved: &[u8],
    channels: usize,
    bytes_per_sample: usize,
    channel: usize,
    frames: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames * bytes_per_sample);
    for frame in 0..frames {
        let offset = (frame * channels + channel) * bytes_per_sample;
        out.extend_from_slice(&interleaved[offset..offset + bytes_per_sample]);
    }
    out
}

// ---------------------------------------------------------------------------
// onset / repeat / timebase
// ---------------------------------------------------------------------------

/// 一次 onset 检测结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepeatOnset {
    /// 会话 slot 下标（0-based，按播放顺序）。
    pub session_slot: u32,
    /// layout 容器槽位。
    pub container_slot: u32,
    /// repeat 序号（0-based）。
    pub repeat: u32,
    /// 期望 render frame。
    pub expected_render_frame: u64,
    /// 期望 capture sample（seed + render frame）。
    pub expected_capture_sample: f64,
    /// 实测 onset capture sample（阈值插值 + 解析模板偏置扣除；不是绝对到达时刻，
    /// 房间/麦克风滤波残余偏置归 T49 绝对时轴处理）。
    pub onset_capture_sample: f64,
    /// 检测方法。
    pub method: OnsetMethod,
    /// 是否用宽带包络把带限 LF 检出点精化为起点（false = 仅 LF 检出点）。
    pub wide_refined: bool,
    /// 搜索窗内局部峰值（dBFS）。
    pub local_peak_dbfs: f64,
    /// 搜索窗起点（capture 样本，含）。
    pub search_start_sample: u64,
    /// 搜索窗终点（capture 样本，含）。
    pub search_end_sample: u64,
}

/// 一个会话槽位的捕获 diag（对应 T48 TimebaseDiag 的 producer 侧）。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotDiag {
    /// 会话 slot 下标（0-based，= T48 diag 的 slot）。
    pub session_slot: u32,
    /// layout 容器槽位。
    pub container_slot: u32,
    /// 角色 id。
    pub speaker: String,
    /// 播放设备通道号。
    pub channel: u32,
    /// 首遍扫频起点 render frame。
    pub render_frame_index: u64,
    /// repeat 次数。
    pub repeats: u32,
    /// 名义槽起点（capture 样本 = seed + render_frame_index）。
    pub nominal_start: f64,
    /// 实测首遍起点（capture 样本；无效为 None）。
    pub actual_start: Option<f64>,
    /// actual - nominal（capture 样本；无效为 None）。
    pub lag_samples: Option<f64>,
    /// 每遍实测起点（capture 样本；长度 = 检出遍数）。
    pub repeat_starts: Vec<f64>,
    /// 是否 invalid（检出遍数不足 repeats）。
    pub onset_invalid: bool,
}

/// 残余相位 15 度判定可用性（ACCEPTANCE §0.2.4；只判 NOT_APPLICABLE，不伪造 PASS）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResidualPhaseClaim {
    /// 是否满足可声明残余相位 <15 度的时间跨度/sigma 条件。
    pub applicable: bool,
    /// 采用的口径。
    pub criterion: ResidualPhaseCriterion,
    /// 实测跨度（秒）。
    pub span_seconds: f64,
    /// sigma_ppm（不可估计为 None）。
    pub sigma_ppm: Option<f64>,
    /// 要求 sigma_ppm 上限（0.4）。
    pub required_sigma_ppm: f64,
}

/// clock 估计结果（repeat-interval 主估计 + T19 交叉复核）。
#[derive(Debug, Clone, PartialEq)]
pub struct ClockFit {
    /// 实测 ppm（正 = capture 采样率快于 render 时钟；不可估计为 None）。
    pub clock_ppm: Option<f64>,
    /// ppm 1 sigma（<2 个间隔不可估计为 None）。
    pub clock_ppm_uncertainty_ppm: Option<f64>,
    /// |ppm| > 2 sigma（ACCEPTANCE §0.2.2；不足/无效为 false）。
    pub drift_significant: bool,
    /// clock 估计是否有效（有效间隔 >=2、量级/sigma/残差均在界内）。
    pub clock_ppm_ok: bool,
    /// 校正声明（只有显著且有效才 Resample；N=1 永不校正）。
    pub correction: DriftCorrection,
    /// capture samples per render frame（1 + ppm/1e6；不可估计为 None）。
    pub samples_per_frame: Option<f64>,
    /// 参与 ppm 的有效间隔数。
    pub intervals: usize,
    /// 被拒的间隔数（超容差/超 sanity/非递增）。
    pub rejected_intervals: usize,
    /// 拟合残差 RMS（capture 样本）。
    pub residual_rms_samples: f64,
    /// 首末有效 onset 的跨度（秒）。
    pub span_seconds: f64,
    /// sanity 上限（100 ppm；不是漂移证据）。
    pub sanity_limit_ppm: f64,
    /// 显著性因子（2 sigma）。
    pub significance_factor: f64,
    /// 残余相位可用性声明。
    pub residual_phase_claim: ResidualPhaseClaim,
    /// T19 PpmEstimator 交叉复核（>=4 个有效 onset 且 epoch_start 已知时）。
    pub t19_cross_check: Option<PpmEstimate>,
}

/// 采集时基报告（render frame 与 capture sample 映射 + repeat + clock）。
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureTimebase {
    /// schema。
    pub schema: &'static str,
    /// session id。
    pub session_id: String,
    /// layout id。
    pub layout: String,
    /// host epoch id。
    pub epoch_id: u64,
    /// host graph id。
    pub graph_id: String,
    /// 采集流角色。
    pub stream_role: CaptureStreamRole,
    /// 设备串。
    pub device: String,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// render block 帧数。
    pub block_frames: u16,
    /// 每槽 repeat 次数。
    pub repeats: u32,
    /// seed：capture sample = seed + render_frame（capture 样本）。
    pub capture_seed_samples_at_render_frame0: f64,
    /// 逐槽 diag。
    pub slots: Vec<SlotDiag>,
    /// 全部有效 onset（按时间顺序）。
    pub onsets: Vec<RepeatOnset>,
    /// clock 估计。
    pub clock: ClockFit,
    /// invalid 槽位数。
    pub onsets_invalid_slots: u32,
    /// 全部槽位 onset 是否有效。
    pub all_slots_valid: bool,
    /// onset 参数实际值。
    pub onset_config: OnsetConfig,
}

impl CaptureTimebase {
    /// 转成 T48 MicClock（clock 不可估计为 None；T63 据此拒交付）。
    pub fn mic_clock(&self) -> Option<MicClock> {
        let ppm = self.clock.clock_ppm?;
        let sigma = self.clock.clock_ppm_uncertainty_ppm?;
        Some(MicClock {
            clock_ppm: ppm,
            clock_ppm_uncertainty_ppm: sigma,
            drift_significant: self.clock.drift_significant,
            clock_ppm_ok: self.clock.clock_ppm_ok,
            correction: self.clock.correction,
        })
    }

    /// 生成 T48 TimebaseDiag（逐 slot；clock 不可估计时返回错误，不伪造 diag）。
    pub fn timebase_diags(&self, capture_file: &str) -> Result<Vec<TimebaseDiag>, CaptureError> {
        let clock = self.mic_clock().ok_or(CaptureError::ClockNotEstimable {
            intervals: self.clock.intervals,
        })?;
        let mut diags = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            diags.push(TimebaseDiag {
                session_id: self.session_id.clone(),
                epoch_id: self.epoch_id,
                graph_id: self.graph_id.clone(),
                speaker: slot.speaker.clone(),
                channel: slot.channel,
                slot: slot.session_slot,
                capture_file: capture_file.to_string(),
                render_frame_index: slot.render_frame_index,
                capture_sample_index: slot.actual_start.unwrap_or(f64::NAN).round() as u64,
                nominal_start: slot.nominal_start,
                actual_start: slot.actual_start.unwrap_or(f64::NAN),
                lag_samples: slot.lag_samples.unwrap_or(f64::NAN),
                repeat_starts: slot.repeat_starts.clone(),
                repeats: slot.repeats,
                clock,
                onset_invalid: slot.onset_invalid,
            });
        }
        Ok(diags)
    }

    /// 时基报告 JSON（确定性；无第三方依赖）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(4096);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_str(&mut out, self.schema);
        out.push_str(",\"session_id\":");
        push_json_str(&mut out, &self.session_id);
        out.push_str(",\"layout\":");
        push_json_str(&mut out, &self.layout);
        let _ = write!(out, ",\"epoch_id\":{},\"graph_id\":", self.epoch_id);
        push_json_str(&mut out, &self.graph_id);
        out.push_str(",\"stream_role\":");
        push_json_str(&mut out, self.stream_role.as_str());
        out.push_str(",\"device\":");
        push_json_str(&mut out, &self.device);
        let _ = write!(
            out,
            ",\"sample_rate_hz\":{},\"block_frames\":{},\"repeats\":{},\"capture_seed_samples_at_render_frame0\":",
            self.sample_rate_hz, self.block_frames, self.repeats
        );
        push_json_number(&mut out, self.capture_seed_samples_at_render_frame0);
        out.push_str(",\"onset_config\":{\"search_half_window_seconds\":");
        push_json_number(&mut out, self.onset_config.search_half_window_seconds);
        out.push_str(",\"search_forward_seconds\":");
        push_json_number(&mut out, self.onset_config.search_forward_seconds);
        out.push_str(",\"threshold_fraction\":");
        push_json_number(&mut out, self.onset_config.threshold_fraction);
        out.push_str(",\"noise_gate_db\":");
        push_json_number(&mut out, self.onset_config.noise_gate_db);
        out.push_str(",\"method\":");
        push_json_str(&mut out, OnsetMethod::BandlimitedLfThreshold.as_str());
        out.push_str(",\"lf_band_hz\":[");
        push_json_number(&mut out, ONSET_LF_BAND_LOW_HZ);
        out.push(',');
        push_json_number(&mut out, ONSET_LF_BAND_HIGH_HZ);
        out.push(']');
        out.push_str(",\"reference_lookahead_seconds\":");
        push_json_number(&mut out, ONSET_REFERENCE_LOOKAHEAD_SECONDS);
        out.push_str(",\"interval_tolerance_fraction\":");
        push_json_number(&mut out, self.onset_config.interval_tolerance_fraction);
        out.push_str(",\"interval_tolerance_samples\":");
        push_json_number(&mut out, self.onset_config.interval_tolerance_samples);
        out.push('}');
        out.push_str(",\"slots\":[");
        for (index, slot) in self.slots.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            let _ = write!(
                out,
                "\"session_slot\":{},\"container_slot\":{},\"speaker\":",
                slot.session_slot, slot.container_slot
            );
            push_json_str(&mut out, &slot.speaker);
            let _ = write!(
                out,
                ",\"channel\":{},\"render_frame_index\":{},\"repeats\":{},\"nominal_start\":",
                slot.channel, slot.render_frame_index, slot.repeats
            );
            push_json_number(&mut out, slot.nominal_start);
            out.push_str(",\"actual_start\":");
            push_json_opt_number(&mut out, slot.actual_start);
            out.push_str(",\"lag_samples\":");
            push_json_opt_number(&mut out, slot.lag_samples);
            out.push_str(",\"repeat_starts\":[");
            for (repeat_index, onset) in slot.repeat_starts.iter().enumerate() {
                if repeat_index > 0 {
                    out.push(',');
                }
                push_json_number(&mut out, *onset);
            }
            let _ = write!(out, "],\"onset_invalid\":{}}}", slot.onset_invalid);
        }
        out.push_str("],\"clock\":{\"clock_ppm\":");
        push_json_opt_number(&mut out, self.clock.clock_ppm);
        out.push_str(",\"clock_ppm_uncertainty_ppm\":");
        push_json_opt_number(&mut out, self.clock.clock_ppm_uncertainty_ppm);
        let _ = write!(
            out,
            ",\"drift_significant\":{},\"clock_ppm_ok\":{},\"correction\":",
            self.clock.drift_significant, self.clock.clock_ppm_ok
        );
        push_json_str(&mut out, self.clock.correction.as_str());
        out.push_str(",\"samples_per_frame\":");
        push_json_opt_number(&mut out, self.clock.samples_per_frame);
        let _ = write!(
            out,
            ",\"intervals\":{},\"rejected_intervals\":{},\"residual_rms_samples\":{},\"span_seconds\":{},\"sanity_limit_ppm\":{},\"significance_factor\":{}",
            self.clock.intervals,
            self.clock.rejected_intervals,
            json_number(self.clock.residual_rms_samples),
            json_number(self.clock.span_seconds),
            json_number(self.clock.sanity_limit_ppm),
            json_number(self.clock.significance_factor)
        );
        out.push_str(",\"residual_phase_claim\":{\"applicable\":");
        let _ = write!(out, "{}", self.clock.residual_phase_claim.applicable);
        out.push_str(",\"criterion\":");
        push_json_str(&mut out, self.clock.residual_phase_claim.criterion.as_str());
        out.push_str(",\"span_seconds\":");
        push_json_number(&mut out, self.clock.residual_phase_claim.span_seconds);
        out.push_str(",\"sigma_ppm\":");
        push_json_opt_number(&mut out, self.clock.residual_phase_claim.sigma_ppm);
        out.push_str(",\"required_sigma_ppm\":");
        push_json_number(&mut out, self.clock.residual_phase_claim.required_sigma_ppm);
        out.push('}');
        if let Some(estimate) = &self.clock.t19_cross_check {
            let _ = write!(
                out,
                ",\"t19_cross_check\":{{\"frame_ppm_ppb\":{},\"frame_sigma_ppb\":{},\"frame_residual_rms_ns\":{},\"samples\":{},\"frame_valid\":{},\"significant\":{}}}",
                estimate.frame_ppm_ppb,
                estimate.frame_sigma_ppb,
                estimate.frame_residual_rms_ns,
                estimate.samples,
                estimate.frame_valid,
                estimate.significant
            );
        } else {
            out.push_str(",\"t19_cross_check\":null");
        }
        let _ = write!(
            out,
            "}},\"onsets_invalid_slots\":{},\"all_slots_valid\":{}}}",
            self.onsets_invalid_slots, self.all_slots_valid
        );
        out
    }
}

struct OnsetHit {
    onset_sample: f64,
    local_peak_dbfs: f64,
    search_start: u64,
    search_end: u64,
    /// 是否用宽带包络精化了起点（false = 仅带限 LF 包络检出点，绝对时刻精度较差）。
    wide_refined: bool,
}

// ---------------------------------------------------------------------------
// onset 包络（T62 onset-fix：带限 LF + 噪声底双门 + 宽带精化）
// ---------------------------------------------------------------------------

/// 二阶 RBJ 双二阶（直接形式 I；系数已按 a0 归一化）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// 双二阶滤波状态（逐样本流式处理，避免为长采集分配中间 f64 缓冲）。
#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// RBJ 二阶高通（Q = 1/sqrt(2)，12 dB/oct）。
    fn highpass(cutoff_hz: f64, sample_rate_hz: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * cutoff_hz / sample_rate_hz;
        let alpha = w0.sin() * std::f64::consts::FRAC_1_SQRT_2;
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) * 0.5 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) * 0.5 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// RBJ 二阶低通（Q = 1/sqrt(2)，12 dB/oct）。
    fn lowpass(cutoff_hz: f64, sample_rate_hz: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * cutoff_hz / sample_rate_hz;
        let alpha = w0.sin() * std::f64::consts::FRAC_1_SQRT_2;
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) * 0.5 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) * 0.5 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn step(&self, state: &mut BiquadState, input: f64) -> f64 {
        let output = self.b0 * input + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;
        state.x2 = state.x1;
        state.x1 = input;
        state.y2 = state.y1;
        state.y1 = output;
        output
    }
}

/// onset 检测包络：可选带限（LF）或宽带；整流后一阶平滑。
///
/// - band = Some((low, high))：二阶高通 + 二阶低通（40–200 Hz），避开 20 Hz 极低能量
///   与 HF 峰值对阈值的影响（T62 NoOnsets 根因）；
/// - band = None：宽带包络，用于把带限 LF 的检出点精化到真实起点（绝对时刻仍由
///   T49 绝对时轴负责，本模块只保证相对 repeat 时基）。
fn onset_envelope(samples: &[f32], sample_rate_hz: f64, band: Option<(f64, f64)>) -> Vec<f32> {
    let filters = band.map(|(low, high)| {
        (
            Biquad::highpass(low, sample_rate_hz),
            Biquad::lowpass(high, sample_rate_hz),
        )
    });
    let mut high_state = BiquadState::default();
    let mut low_state = BiquadState::default();
    let alpha = 1.0 - (-1.0 / (ONSET_ENVELOPE_TAU_SECONDS * sample_rate_hz)).exp();
    let mut envelope = Vec::with_capacity(samples.len());
    let mut smoothed = 0.0f64;
    for sample in samples {
        let mut value = *sample as f64;
        if let Some((high, low)) = filters {
            value = high.step(&mut high_state, value);
            value = low.step(&mut low_state, value);
        }
        smoothed += alpha * (value.abs() - smoothed);
        envelope.push(smoothed as f32);
    }
    envelope
}

/// 带限 LF 包的稳健噪声底：采集开头（expected 前 0.3 s 以前）50 ms 块最大值的 20 分位；
/// 开头太短时退回全采集块最大值的 10 分位。返回线性幅度。
fn onset_noise_floor(envelope: &[f32], sample_rate_hz: f64, first_expected: f64) -> f64 {
    if envelope.is_empty() {
        return 0.0;
    }
    let block = ((0.05 * sample_rate_hz).round() as usize).max(1);
    let begin = ((0.35 * sample_rate_hz).round() as usize).min(envelope.len() / 2);
    let hint = first_expected.max(0.0) as usize;
    let end = hint
        .saturating_sub((0.3 * sample_rate_hz).round() as usize)
        .max(begin);
    let mut blocks = Vec::new();
    let mut index = begin;
    while index + block <= envelope.len() && index < end {
        let mut peak = 0.0f64;
        for value in &envelope[index..index + block] {
            peak = peak.max(*value as f64);
        }
        blocks.push(peak);
        index += block;
    }
    if blocks.len() < 4 {
        blocks.clear();
        let mut index = begin;
        while index + block <= envelope.len() {
            let mut peak = 0.0f64;
            for value in &envelope[index..index + block] {
                peak = peak.max(*value as f64);
            }
            blocks.push(peak);
            index += block;
        }
    }
    if blocks.is_empty() {
        return 0.0;
    }
    blocks.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let fraction = if blocks.len() >= 20 { 0.20 } else { 0.10 };
    blocks[(((blocks.len() - 1) as f64) * fraction).round() as usize]
}

/// onset 检测所需的预计算包络（每个采集流只算一次）。
struct OnsetEnvelope {
    /// 带限 LF 包络（40–200 Hz；阈值检测参考）。
    lf: Vec<f32>,
    /// 宽带包络（起点精化）。
    wide: Vec<f32>,
    /// LF 包稳健噪声底（线性幅度）。
    lf_noise: f64,
}

impl OnsetEnvelope {
    fn build(samples: &[f32], sample_rate_hz: u32, first_expected: f64) -> Option<Self> {
        if samples.len() < 64 || sample_rate_hz == 0 {
            return None;
        }
        let sample_rate = sample_rate_hz as f64;
        let lf = onset_envelope(
            samples,
            sample_rate,
            Some((ONSET_LF_BAND_LOW_HZ, ONSET_LF_BAND_HIGH_HZ)),
        );
        let wide = onset_envelope(samples, sample_rate, None);
        let lf_noise = onset_noise_floor(&lf, sample_rate, first_expected);
        Some(Self { lf, wide, lf_noise })
    }
}

/// 在 [start, end]（含）内找第一个连续 hold 个样本都 >= gate 的过阈点，并在最后一个
/// 低于阈的样本与本样本之间做线性插值（子样本精度，repeat-interval ppm 需要）；
/// 跳过窗口起点处仍在阈上的残响尾（避免把上一遍尾巴当 onset）。
fn first_crossing(envelope: &[f32], start: u64, end: u64, gate: f64, hold: u64) -> Option<f64> {
    if envelope.is_empty() || end >= envelope.len() as u64 {
        return None;
    }
    let mut index = start;
    while index <= end && envelope[index as usize] as f64 >= gate {
        index = index.saturating_add(1);
    }
    while index <= end {
        if envelope[index as usize] as f64 >= gate {
            let limit = index.saturating_add(hold).min(end.saturating_add(1));
            let mut held = true;
            let mut probe = index;
            while probe < limit {
                if (envelope[probe as usize] as f64) < gate {
                    held = false;
                    break;
                }
                probe += 1;
            }
            if held {
                let current = envelope[index as usize] as f64;
                let previous = if index > 0 {
                    envelope[index as usize - 1] as f64
                } else {
                    0.0
                };
                let delta = current - previous;
                let fraction = if delta > 0.0 {
                    ((gate - previous) / delta).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let interpolated = (index as f64 - 1.0 + fraction).max(start as f64);
                return Some(interpolated);
            }
        }
        index = index.saturating_add(1);
    }
    None
}

/// 解析扫频模板样本（与 T61 sweep.rs 相同的 log-sweep 相位 + raised-cosine fade 公式）。
///
/// 只用于计算阈值法的固定系统偏置：阈值交叉点相对真实起点有一个由波形/阈值决定的常数
/// 偏移（低频段可高达数百样本），用同参数模板离线算出来后在 onset 中扣掉，使
/// actual_start 不引入该偏置（房间/麦克风滤波造成的残余偏置由 T49 绝对时轴处理）。
fn sweep_template_sample(grid: &RepeatGrid, t: f64) -> f64 {
    let sweep_frames = grid.sweep_frames as f64;
    if !(t >= 0.0) || t >= sweep_frames {
        return 0.0;
    }
    let fade = grid.fade_frames as f64;
    let sample_rate = grid.sample_rate_hz as f64;
    let duration = sweep_frames / sample_rate;
    let log_ratio = (grid.end_hz / grid.start_hz).ln();
    let time = t / sample_rate;
    let phase = 2.0 * std::f64::consts::PI * grid.start_hz * duration / log_ratio
        * ((time / duration * log_ratio).exp() - 1.0);
    let envelope = if fade > 1.0 && t < fade {
        0.5 * (1.0 - (std::f64::consts::PI * t / (fade - 1.0)).cos())
    } else if fade > 1.0 && t + fade >= sweep_frames {
        let local = t - (sweep_frames - fade);
        0.5 * (1.0 + (std::f64::consts::PI * local / (fade - 1.0)).cos())
    } else {
        1.0
    };
    grid.peak_linear * envelope * phase.sin()
}

/// 阈值法 onset 的固定系统偏置（样本）：在同参数解析模板上跑一次检测器，
/// 得到“检出点 - 真实起点”的常数偏移。返回 0 表示模板不可用。
///
/// 模板按检测器窗口同构构造：前置 search_half_window 静音（使 expected 对齐模板里的
/// sweep 起点），再放同参数 log-sweep，尾部补静音避免截断振铃进入参考窗。模板无噪声，
/// 因此噪声底显式置 0（与无噪声合成采集一致）。
fn compute_onset_template_bias(grid: &RepeatGrid, config: &OnsetConfig) -> f64 {
    if grid.sweep_frames == 0 || !finite_positive(grid.peak_linear) || grid.sample_rate_hz == 0 {
        return 0.0;
    }
    let sample_rate = grid.sample_rate_hz as f64;
    let half = ((config.search_half_window_seconds * sample_rate).round() as u64).max(1) as usize;
    let tail = ((0.1 * sample_rate).round() as u64).max(1) as usize;
    let wanted = ((ONSET_REFERENCE_LOOKAHEAD_SECONDS + 0.1) * sample_rate).round() as u64;
    let length = (grid.sweep_frames).min(wanted) as usize;
    if length < 16 {
        return 0.0;
    }
    let mut template = vec![0.0f32; half + length + tail];
    for index in 0..length {
        template[half + index] = sweep_template_sample(grid, index as f64) as f32;
    }
    let mut envelope = match OnsetEnvelope::build(&template, grid.sample_rate_hz, half as f64) {
        Some(envelope) => envelope,
        None => return 0.0,
    };
    envelope.lf_noise = 0.0;
    match detect_onset(&envelope, half as f64, 0, config, grid.sample_rate_hz) {
        Some(hit) if hit.onset_sample.is_finite() => (hit.onset_sample - half as f64).max(0.0),
        _ => 0.0,
    }
}

/// 带限 LF 包络阈值检测（噪声底双门）+ 宽带包络起点精化。
///
/// 阈值 = max(LF 参考峰值 x threshold_fraction, LF 噪声底 + noise_gate_db)；参考峰值取
/// [expected - half, expected + reference_lookahead] 的 LF 包最大值（与 sweep 起始频率
/// 解耦的固定前视，不再被 ±half 窗内的 HF/电平涨落拖动）。LF 包第一处过阈给出粗检点，
/// 再从搜索窗起点到粗检点之间找宽带包第一处过阈作为真实起点（粗检只负责“有没有”
/// 与“候选区”，宽带包负责绝对时刻；模板偏置用同一条路径扣掉常数系统偏移）。
fn detect_onset(
    envelope: &OnsetEnvelope,
    expected: f64,
    lower_bound: u64,
    config: &OnsetConfig,
    sample_rate_hz: u32,
) -> Option<OnsetHit> {
    if envelope.lf.is_empty() || envelope.wide.len() != envelope.lf.len() || sample_rate_hz == 0 {
        return None;
    }
    let sample_rate = sample_rate_hz as f64;
    let half = ((config.search_half_window_seconds * sample_rate).round() as u64).max(1);
    let forward = ((config.search_forward_seconds * sample_rate).round() as u64).max(1);
    let last = (envelope.lf.len() - 1) as u64;
    let center = expected.max(0.0).round() as u64;
    let start = center.saturating_sub(half).max(lower_bound);
    if start > last {
        return None;
    }
    let end = center.saturating_add(forward).min(last);
    if end <= start {
        return None;
    }
    let look = ((ONSET_REFERENCE_LOOKAHEAD_SECONDS * sample_rate).round() as u64).max(1);
    let reference_end = center.saturating_add(look).min(last);
    let mut lf_peak = 0.0f64;
    for index in start..=reference_end {
        let value = envelope.lf[index as usize] as f64;
        if value > lf_peak {
            lf_peak = value;
        }
    }
    if !lf_peak.is_finite() || lf_peak <= ONSET_ENVELOPE_MIN_PEAK {
        return None;
    }
    let noise_floor = envelope.lf_noise * 10.0f64.powf(config.noise_gate_db / 20.0);
    let gate = (lf_peak * config.threshold_fraction).max(noise_floor);
    if !gate.is_finite() || gate <= 0.0 {
        return None;
    }
    let lf_hold = ((ONSET_LF_HOLD_SECONDS * sample_rate).round() as u64).max(1);
    let coarse = first_crossing(&envelope.lf, start, end, gate, lf_hold)?;
    let coarse_index = coarse.round().max(0.0) as u64;
    let wide_hold = ((ONSET_WIDE_HOLD_SECONDS * sample_rate).round() as u64).max(1);
    let mut onset = coarse;
    let mut wide_refined = false;
    if let Some(wide) = first_crossing(&envelope.wide, start, coarse_index, gate, wide_hold) {
        onset = wide;
        wide_refined = true;
    }
    Some(OnsetHit {
        onset_sample: onset,
        local_peak_dbfs: dbfs_of(lf_peak),
        search_start: start,
        search_end: end,
        wide_refined,
    })
}

fn render_frame_of_mono(mono_ns: u64, epoch_start_mono_ns: u64, sample_rate_hz: u32) -> f64 {
    if sample_rate_hz == 0 {
        return 0.0;
    }
    let delta = mono_ns as f64 - epoch_start_mono_ns as f64;
    delta * sample_rate_hz as f64 / 1_000_000_000.0
}

/// seed = render frame 0 对应的采集 sample 索引（采集先起 => 正值；采集晚于 render 0
/// => 负值）。字段本身即 seed 语义（与 `AudioCaptureResult::seed_render_frame` /
/// `FakeEngine` 一致），**不得再取负号**：修复前这里返回 `-capture_start_render_frame`，
/// 导致 `compute_noise_floor` 的 onset 被算成 ~0.3 s 并 clamp 到采集开头，正好吃进
/// apply-host 退出时的流切换 pop（见 evidence/calib-chain-20261121/g0-g1-20261121/
/// snr-cliff.json）。
fn seed_from_stream(stream: &CapturedStream) -> Result<f64, CaptureError> {
    if let Some(seed) = stream.capture_start_render_frame {
        return Ok(seed);
    }
    if stream.epoch_start_mono_ns > 0 {
        if let Some(block) = stream.blocks.first() {
            let render_frame = render_frame_of_mono(
                block.mono_ns,
                stream.epoch_start_mono_ns,
                stream.sample_rate_hz,
            );
            return Ok(block.start_sample as f64 - render_frame);
        }
    }
    Err(CaptureError::InvalidConfig {
        field: "capture_start_render_frame",
        detail: "timebase needs capture_start_render_frame or epoch_start_mono_ns + block stamps"
            .to_string(),
    })
}

/// 稳健噪声底（dBFS）：不再用 "固定前静音均值"（修复前窗口被 clamp 到 [0,0.308 s)
/// 吃掉 apply-host 退出 pop，见 snr-cliff.json），而是走 `noise_est` 的分块 RMS
/// 中位数 + 瞬态剔除；post-sweep 尾段与 repeat 间隙都进入候选。
fn compute_noise_floor(
    samples: &[f32],
    seed: f64,
    grid: &RepeatGrid,
    sample_rate_hz: u32,
) -> Option<f64> {
    let first_frame = grid.first_render_frame()?;
    let expected_onset = seed + first_frame as f64;
    let estimate = crate::noise_est::estimate(
        samples,
        sample_rate_hz,
        grid.sweep_frames,
        Some(expected_onset),
        &crate::noise_est::RobustSnrConfig::default(),
    )?;
    Some(estimate.noise_rms_dbfs)
}

fn compute_clock_fit(
    onsets: &[RepeatOnset],
    grid: &RepeatGrid,
    sample_rate_hz: u32,
    config: &OnsetConfig,
) -> ClockFit {
    let mut interval_ppm: Vec<f64> = Vec::new();
    let mut rejected_intervals = 0usize;
    for slot in &grid.slots {
        let mut slot_onsets: Vec<&RepeatOnset> = onsets
            .iter()
            .filter(|onset| onset.container_slot == slot.container_slot)
            .collect();
        slot_onsets.sort_by_key(|onset| onset.repeat);
        for pair in slot_onsets.windows(2) {
            let first = pair[0];
            let second = pair[1];
            let interval_frames =
                second.expected_render_frame as f64 - first.expected_render_frame as f64;
            if interval_frames <= 0.0 {
                rejected_intervals += 1;
                continue;
            }
            let interval_capture = second.onset_capture_sample - first.onset_capture_sample;
            let tolerance = config.interval_tolerance(interval_frames as u64);
            if (interval_capture - interval_frames).abs() > tolerance {
                rejected_intervals += 1;
                continue;
            }
            let ppm = (interval_capture / interval_frames - 1.0) * 1.0e6;
            if !ppm.is_finite() || ppm.abs() > DRIFT_SANITY_LIMIT_PPM {
                rejected_intervals += 1;
                continue;
            }
            interval_ppm.push(ppm);
        }
    }

    let span_seconds = if onsets.len() >= 2 {
        (onsets[onsets.len() - 1].onset_capture_sample - onsets[0].onset_capture_sample)
            / sample_rate_hz.max(1) as f64
    } else {
        0.0
    };

    let mut fit = ClockFit {
        clock_ppm: None,
        clock_ppm_uncertainty_ppm: None,
        drift_significant: false,
        clock_ppm_ok: false,
        correction: DriftCorrection::None,
        samples_per_frame: None,
        intervals: interval_ppm.len(),
        rejected_intervals,
        residual_rms_samples: 0.0,
        span_seconds,
        sanity_limit_ppm: DRIFT_SANITY_LIMIT_PPM,
        significance_factor: DRIFT_SIGNIFICANCE_SIGMA,
        residual_phase_claim: ResidualPhaseClaim {
            applicable: false,
            criterion: ResidualPhaseCriterion::DefaultSpan10Point4,
            span_seconds,
            sigma_ppm: None,
            required_sigma_ppm: RESIDUAL_PHASE_SIGMA_MAX_PPM,
        },
        t19_cross_check: None,
    };

    if interval_ppm.is_empty() {
        return fit;
    }
    let mean = interval_ppm.iter().sum::<f64>() / interval_ppm.len() as f64;
    fit.clock_ppm = Some(mean);
    fit.samples_per_frame = Some(1.0 + mean / 1.0e6);
    let sigma = if interval_ppm.len() >= 2 {
        let variance = interval_ppm
            .iter()
            .map(|value| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>()
            / (interval_ppm.len() as f64 - 1.0);
        (variance / interval_ppm.len() as f64)
            .sqrt()
            .max(PPM_SIGMA_FLOOR_PPM)
    } else {
        f64::INFINITY
    };
    if sigma.is_finite() {
        fit.clock_ppm_uncertainty_ppm = Some(sigma);
    }
    let samples_per_frame = 1.0 + mean / 1.0e6;
    let mut squared = 0.0f64;
    let mut counted = 0usize;
    for slot in &grid.slots {
        let mut slot_onsets: Vec<&RepeatOnset> = onsets
            .iter()
            .filter(|onset| onset.container_slot == slot.container_slot)
            .collect();
        slot_onsets.sort_by_key(|onset| onset.repeat);
        for pair in slot_onsets.windows(2) {
            let interval_frames =
                pair[1].expected_render_frame as f64 - pair[0].expected_render_frame as f64;
            if interval_frames <= 0.0 {
                continue;
            }
            let interval_capture = pair[1].onset_capture_sample - pair[0].onset_capture_sample;
            let tolerance = config.interval_tolerance(interval_frames as u64);
            if (interval_capture - interval_frames).abs() > tolerance {
                continue;
            }
            let predicted = interval_frames * samples_per_frame;
            let residual = interval_capture - predicted;
            squared += residual * residual;
            counted += 1;
        }
    }
    if counted > 0 {
        fit.residual_rms_samples = (squared / counted as f64).sqrt();
    }
    let ok = interval_ppm.len() >= 2
        && mean.is_finite()
        && mean.abs() <= DRIFT_SANITY_LIMIT_PPM
        && sigma.is_finite()
        && sigma <= CLOCK_SIGMA_MAX_PPM
        && fit.residual_rms_samples <= ONSET_RESIDUAL_MAX_SAMPLES;
    fit.clock_ppm_ok = ok;
    if ok {
        let significant = mean.abs() > DRIFT_SIGNIFICANCE_SIGMA * sigma;
        fit.drift_significant = significant;
        fit.correction = if significant {
            DriftCorrection::Resample
        } else {
            DriftCorrection::None
        };
    }
    let criterion = if span_seconds >= RESIDUAL_PHASE_MIN_SPAN_SECONDS {
        ResidualPhaseCriterion::SixtySeconds
    } else if grid.repeats >= RESIDUAL_PHASE_MIN_REPEATS {
        ResidualPhaseCriterion::FiveRepeats
    } else {
        ResidualPhaseCriterion::DefaultSpan10Point4
    };
    let applicable = ok
        && sigma.is_finite()
        && sigma <= RESIDUAL_PHASE_SIGMA_MAX_PPM
        && (span_seconds >= RESIDUAL_PHASE_DEFAULT_SPAN_SECONDS
            || grid.repeats >= RESIDUAL_PHASE_MIN_REPEATS);
    fit.residual_phase_claim = ResidualPhaseClaim {
        applicable,
        criterion,
        span_seconds,
        sigma_ppm: if sigma.is_finite() { Some(sigma) } else { None },
        required_sigma_ppm: RESIDUAL_PHASE_SIGMA_MAX_PPM,
    };
    fit
}

fn t19_cross_check_observations(
    onsets: &[RepeatOnset],
    epoch_start_mono_ns: u64,
    sample_rate_hz: u32,
) -> Option<Vec<ClockObservation>> {
    if onsets.len() < T19_CROSS_CHECK_MIN_POINTS || sample_rate_hz == 0 || epoch_start_mono_ns == 0
    {
        return None;
    }
    let mut observations = Vec::with_capacity(onsets.len());
    for onset in onsets {
        let mono_ns = (epoch_start_mono_ns as f64
            + onset.expected_render_frame as f64 * 1_000_000_000.0 / sample_rate_hz as f64)
            .round() as u64;
        observations.push(ClockObservation::motu(
            onset.onset_capture_sample.max(0.0).round() as u64,
            mono_ns,
        ));
    }
    Some(observations)
}

/// 估计一个采集流相对 host render frame 的时基（repeat-interval 主估计）。
///
/// 设备域/plan 的不混设备校验在 finalize_acoustic_capture /
/// finalize_electrical_loopback 中完成；本函数只做纯数据估计（可离线单测）。
pub fn estimate_capture_timebase(
    stream: &CapturedStream,
    grid: &RepeatGrid,
    config: &OnsetConfig,
) -> Result<CaptureTimebase, CaptureError> {
    config.validate()?;
    if stream.sample_rate_hz != grid.sample_rate_hz {
        return Err(CaptureError::UnsupportedSampleRate {
            actual: stream.sample_rate_hz,
            detail: format!(
                "capture rate must equal timeline rate {}",
                grid.sample_rate_hz
            ),
        });
    }
    if stream.session_id != grid.session_id {
        return Err(CaptureError::InvalidTimeline {
            detail: format!(
                "capture session {:?} != timeline session {:?}",
                stream.session_id, grid.session_id
            ),
        });
    }
    if stream.epoch_id != grid.epoch_id {
        return Err(CaptureError::InvalidTimeline {
            detail: format!(
                "capture epoch {} != timeline epoch {}",
                stream.epoch_id, grid.epoch_id
            ),
        });
    }
    if stream.graph_id != grid.graph_id {
        return Err(CaptureError::InvalidTimeline {
            detail: "capture graph_id != timeline graph_id".to_string(),
        });
    }
    if stream.samples.is_empty() {
        return Err(CaptureError::InvalidBlock {
            block: 0,
            detail: "capture has no samples".to_string(),
        });
    }
    let seed = seed_from_stream(stream)?;
    let sample_rate = stream.sample_rate_hz as f64;
    let margin = 0.5 * sample_rate;
    let first_expected = seed
        + grid
            .first_render_frame()
            .ok_or(CaptureError::InvalidTimeline {
                detail: "timeline has no slots".to_string(),
            })? as f64;
    let last_frame = grid
        .slots
        .last()
        .and_then(|slot| slot.repeat_render_frames.last().copied())
        .ok_or(CaptureError::InvalidTimeline {
            detail: "timeline has no repeat frames".to_string(),
        })?;
    let last_expected = seed + last_frame as f64 + grid.sweep_frames as f64;
    if first_expected < -margin || first_expected > stream.frames as f64 + margin {
        return Err(CaptureError::InvalidTimeline {
            detail: format!(
                "timeline first onset {first_expected} outside capture (frames {})",
                stream.frames
            ),
        });
    }
    if last_expected < -margin || last_expected > stream.frames as f64 + margin {
        return Err(CaptureError::InvalidTimeline {
            detail: format!(
                "timeline last sweep end {last_expected} outside capture (frames {})",
                stream.frames
            ),
        });
    }

    let first_expected = seed
        + grid
            .first_render_frame()
            .ok_or(CaptureError::InvalidTimeline {
                detail: "timeline has no slots".to_string(),
            })? as f64;
    // onset 包络每个采集流只算一次：带限 LF（阈值/参考）+ 宽带（起点精化）+ 噪声底。
    let envelope =
        match OnsetEnvelope::build(&stream.samples, stream.sample_rate_hz, first_expected) {
            Some(envelope) => envelope,
            None => {
                return Err(CaptureError::NoOnsets {
                    slots: grid.slots.len(),
                    detail: "capture too short for onset envelopes (<64 samples)".to_string(),
                })
            }
        };
    let onset_bias = compute_onset_template_bias(grid, config);
    let mut onsets: Vec<RepeatOnset> = Vec::with_capacity(grid.repeat_count());
    let mut slots: Vec<SlotDiag> = Vec::with_capacity(grid.slots.len());
    let mut onsets_invalid_slots = 0u32;
    for (session_slot, slot) in grid.slots.iter().enumerate() {
        let mut repeat_starts: Vec<f64> = Vec::with_capacity(slot.repeat_render_frames.len());
        let mut lower_bound = 0u64;
        for (repeat, render_frame) in slot.repeat_render_frames.iter().enumerate() {
            let expected_capture_sample = seed + *render_frame as f64;
            let hit = detect_onset(
                &envelope,
                expected_capture_sample,
                lower_bound,
                config,
                stream.sample_rate_hz,
            );
            if let Some(hit) = hit {
                let onset_capture_sample = (hit.onset_sample - onset_bias).max(0.0);
                lower_bound = hit.onset_sample.round().max(0.0) as u64 + 1;
                repeat_starts.push(onset_capture_sample);
                onsets.push(RepeatOnset {
                    session_slot: session_slot as u32,
                    container_slot: slot.container_slot,
                    repeat: repeat as u32,
                    expected_render_frame: *render_frame,
                    expected_capture_sample,
                    onset_capture_sample,
                    method: OnsetMethod::BandlimitedLfThreshold,
                    wide_refined: hit.wide_refined,
                    local_peak_dbfs: hit.local_peak_dbfs,
                    search_start_sample: hit.search_start,
                    search_end_sample: hit.search_end,
                });
            }
        }
        let nominal_start = seed + slot.render_frame_index as f64;
        let actual_start = repeat_starts.first().copied();
        let lag_samples = actual_start.map(|actual| actual - nominal_start);
        let onset_invalid = repeat_starts.len() != slot.repeat_render_frames.len();
        if onset_invalid {
            onsets_invalid_slots = onsets_invalid_slots.saturating_add(1);
        }
        slots.push(SlotDiag {
            session_slot: session_slot as u32,
            container_slot: slot.container_slot,
            speaker: slot.speaker.clone(),
            channel: slot.channel,
            render_frame_index: slot.render_frame_index,
            repeats: grid.repeats,
            nominal_start,
            actual_start,
            lag_samples,
            repeat_starts,
            onset_invalid,
        });
    }

    if onsets.is_empty() {
        // 失败原因必须可追溯（T62 onset-fix：带限 LF + 双门参数全部写进错误 detail）。
        return Err(CaptureError::NoOnsets {
            slots: grid.slots.len(),
            detail: format!(
                "onset detector bandlimited_lf {:.0}-{:.0} Hz: threshold_fraction={}, noise_gate_db={}, search=-{}s/+{}s, reference_lookahead={}s, lf_noise={:.1} dBFS",
                ONSET_LF_BAND_LOW_HZ,
                ONSET_LF_BAND_HIGH_HZ,
                config.threshold_fraction,
                config.noise_gate_db,
                config.search_half_window_seconds,
                config.search_forward_seconds,
                ONSET_REFERENCE_LOOKAHEAD_SECONDS,
                dbfs_of(envelope.lf_noise),
            ),
        });
    }

    let mut clock = compute_clock_fit(&onsets, grid, stream.sample_rate_hz, config);
    if let Some(observations) =
        t19_cross_check_observations(&onsets, stream.epoch_start_mono_ns, stream.sample_rate_hz)
    {
        let mut estimator = PpmEstimator::new();
        for observation in observations {
            estimator.push(observation);
        }
        clock.t19_cross_check = estimator.fit(stream.sample_rate_hz);
    }

    Ok(CaptureTimebase {
        schema: UMIK_TIMEBASE_SCHEMA,
        session_id: grid.session_id.clone(),
        layout: grid.layout.clone(),
        epoch_id: grid.epoch_id,
        graph_id: grid.graph_id.clone(),
        stream_role: stream.role,
        device: stream.device.clone(),
        sample_rate_hz: stream.sample_rate_hz,
        block_frames: grid.block_frames,
        repeats: grid.repeats,
        capture_seed_samples_at_render_frame0: seed,
        slots,
        onsets,
        clock,
        onsets_invalid_slots,
        all_slots_valid: onsets_invalid_slots == 0,
        onset_config: *config,
    })
}

// ---------------------------------------------------------------------------
// 报告（finalize：设备分离校验 + 时基 + auxiliary reference）
// ---------------------------------------------------------------------------

/// 声学采集报告（UMIK 主采集 + 可选 MOTU auxiliary electrical reference）。
#[derive(Debug, Clone, PartialEq)]
pub struct AcousticCaptureReport {
    /// schema。
    pub schema: &'static str,
    /// 用途（必须 AcousticRoom）。
    pub purpose: CapturePurpose,
    /// session id。
    pub session_id: String,
    /// 设备分离计划。
    pub plan: ReferencePlan,
    /// UMIK 采集流汇总。
    pub stream: CaptureStreamSummary,
    /// MOTU auxiliary electrical reference 汇总（独立设备；不参与 drift/相位基准）。
    pub auxiliary_reference: Option<CaptureStreamSummary>,
    /// 时基报告。
    pub timebase: CaptureTimebase,
    /// raw 产物登记（sha256 绑定）。
    pub capture_file: CaptureFile,
    /// 麦克风校准精度声明（T67 符号自测前为 NOT_RUN + INCOMPLETE；禁止声称 ±3 dB）。
    pub mic_calibration: MicCalibrationVerification,
}

impl AcousticCaptureReport {
    /// 允许声称的麦克风校准精度（dB）；T67 符号自测完成前恒 None（不得声称 ±3 dB）。
    pub const fn calibration_accuracy_claim_db(&self) -> Option<f64> {
        self.mic_calibration.accuracy_claim_db()
    }

    /// 生成 T48 TimebaseDiag（逐槽）。
    pub fn timebase_diags(&self) -> Result<Vec<TimebaseDiag>, CaptureError> {
        self.timebase
            .timebase_diags(&self.capture_file.relative_path)
    }

    /// 报告 JSON（确定性）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(8192);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_str(&mut out, self.schema);
        out.push_str(",\"purpose\":");
        push_json_str(&mut out, self.purpose.as_str());
        out.push_str(",\"session_id\":");
        push_json_str(&mut out, &self.session_id);
        out.push_str(",\"plan\":");
        write_plan_json(&mut out, &self.plan);
        out.push_str(",\"stream\":");
        write_stream_json(&mut out, &self.stream);
        out.push_str(",\"auxiliary_reference\":");
        match &self.auxiliary_reference {
            Some(summary) => write_stream_json(&mut out, summary),
            None => out.push_str("null"),
        }
        out.push_str(",\"capture_file\":");
        write_capture_file_json(&mut out, &self.capture_file);
        out.push_str(",\"mic_calibration\":");
        write_mic_calibration_json(&mut out, &self.mic_calibration);
        out.push_str(",\"timebase\":");
        out.push_str(&self.timebase.to_json());
        out.push('}');
        out
    }
}

fn write_mic_calibration_json(out: &mut String, verification: &MicCalibrationVerification) {
    out.push('{');
    out.push_str("\"sign_self_test\":");
    push_json_str(out, verification.sign_self_test.as_str());
    out.push_str(",\"accuracy_status\":");
    push_json_str(out, verification.accuracy_status());
    out.push_str(",\"accuracy_db\":");
    push_json_opt_number(out, verification.accuracy_db);
    out.push('}');
}

/// 电学环回采集报告（MOTU 电学 reference 单一设备域；UMIK 不参与）。
#[derive(Debug, Clone, PartialEq)]
pub struct ElectricalLoopbackReport {
    /// schema。
    pub schema: &'static str,
    /// 用途（必须 ElectricalLoopback）。
    pub purpose: CapturePurpose,
    /// session id。
    pub session_id: String,
    /// 设备分离计划。
    pub plan: ReferencePlan,
    /// MOTU 电学 reference 采集流汇总。
    pub stream: CaptureStreamSummary,
    /// 时基报告。
    pub timebase: CaptureTimebase,
    /// raw 产物登记。
    pub capture_file: CaptureFile,
}

impl ElectricalLoopbackReport {
    /// 生成 T48 TimebaseDiag（逐槽）。
    pub fn timebase_diags(&self) -> Result<Vec<TimebaseDiag>, CaptureError> {
        self.timebase
            .timebase_diags(&self.capture_file.relative_path)
    }

    /// 报告 JSON（确定性）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(8192);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_str(&mut out, self.schema);
        out.push_str(",\"purpose\":");
        push_json_str(&mut out, self.purpose.as_str());
        out.push_str(",\"session_id\":");
        push_json_str(&mut out, &self.session_id);
        out.push_str(",\"plan\":");
        write_plan_json(&mut out, &self.plan);
        out.push_str(",\"stream\":");
        write_stream_json(&mut out, &self.stream);
        out.push_str(",\"capture_file\":");
        write_capture_file_json(&mut out, &self.capture_file);
        out.push_str(",\"timebase\":");
        out.push_str(&self.timebase.to_json());
        out.push('}');
        out
    }
}

fn write_plan_json(out: &mut String, plan: &ReferencePlan) {
    out.push('{');
    out.push_str("\"purpose\":");
    push_json_str(out, plan.purpose.as_str());
    out.push_str(",\"umik_device\":");
    match &plan.umik_device {
        Some(device) => push_json_str(out, device),
        None => out.push_str("null"),
    }
    out.push_str(",\"electrical\":");
    match &plan.electrical {
        Some(channel) => {
            out.push('{');
            out.push_str("\"device\":");
            push_json_str(out, &channel.device);
            let _ = write!(
                out,
                ",\"input_channel\":{},\"sample_format\":",
                channel.input_channel
            );
            push_json_str(out, channel.sample_format.as_str());
            out.push('}');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"drift_source\":");
    push_json_str(out, plan.drift_source.as_str());
    out.push_str(",\"phase_reference\":");
    push_json_str(out, plan.phase_reference.as_str());
    out.push('}');
}

fn write_stream_json(out: &mut String, summary: &CaptureStreamSummary) {
    out.push('{');
    out.push_str("\"role\":");
    push_json_str(out, summary.role.as_str());
    out.push_str(",\"device\":");
    push_json_str(out, &summary.device);
    out.push_str(",\"session_id\":");
    push_json_str(out, &summary.session_id);
    let _ = write!(out, ",\"epoch_id\":{},\"graph_id\":", summary.epoch_id);
    push_json_str(out, &summary.graph_id);
    out.push_str(",\"sample_format\":");
    push_json_str(out, summary.sample_format.as_str());
    let _ = write!(
        out,
        ",\"sample_rate_hz\":{},\"channels\":{},\"chunk_frames\":{},\"frames\":{},\"seconds\":{},\"blocks\":{},\"capture_peak_dbfs\":{},\"clipped_samples\":{},\"nonfinite_samples\":{},\"peak_ceiling_dbfs\":{},\"peak_gate_pass\":{},\"raw_sha256\":",
        summary.sample_rate_hz,
        summary.channels,
        summary.chunk_frames,
        summary.frames,
        json_number(summary.seconds),
        summary.blocks,
        json_number(summary.capture_peak_dbfs),
        summary.clipped_samples,
        summary.nonfinite_samples,
        json_number(summary.peak_ceiling_dbfs),
        summary.peak_gate_pass
    );
    push_json_str(out, &summary.raw_sha256);
    out.push_str(",\"noise_floor_dbfs\":");
    push_json_opt_number(out, summary.noise_floor_dbfs);
    out.push_str(",\"mic\":");
    match &summary.mic {
        Some(mic) => {
            out.push('{');
            out.push_str("\"model\":");
            push_json_str(out, &mic.info.model);
            out.push_str(",\"serial\":");
            push_json_str(out, &mic.info.serial);
            out.push_str(",\"cal_file\":");
            push_json_str(out, &mic.cal_file);
            out.push_str(",\"cal_sha256\":");
            push_json_str(out, &mic.info.cal_sha256);
            out.push_str(",\"orientation\":");
            push_json_str(out, &mic.info.orientation);
            out.push_str(",\"clock_domain\":");
            push_json_str(out, &mic.info.clock_domain);
            out.push('}');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"reference_channel\":");
    match summary.reference_channel {
        Some(channel) => {
            let _ = write!(out, "{channel}");
        }
        None => out.push_str("null"),
    }
    out.push('}');
}

fn write_capture_file_json(out: &mut String, file: &CaptureFile) {
    out.push('{');
    out.push_str("\"relative_path\":");
    push_json_str(out, &file.relative_path);
    out.push_str(",\"sha256\":");
    push_json_str(out, &file.sha256);
    let _ = write!(
        out,
        ",\"bytes\":{},\"frames\":{},\"channels\":{},\"sample_rate_hz\":{},\"sample_format\":",
        file.bytes, file.frames, file.channels, file.sample_rate_hz
    );
    push_json_str(out, file.sample_format.as_str());
    out.push('}');
}

fn ensure_role(stream: &CapturedStream, expected: CaptureStreamRole) -> Result<(), CaptureError> {
    if stream.role != expected {
        return Err(CaptureError::MixedReferenceDevices {
            detail: format!(
                "stream role {} does not match required role {}",
                stream.role.as_str(),
                expected.as_str()
            ),
        });
    }
    Ok(())
}

fn ensure_device(stream: &CapturedStream, expected: &str) -> Result<(), CaptureError> {
    if stream.device != expected {
        return Err(CaptureError::MixedReferenceDevices {
            detail: format!(
                "stream device {:?} does not match declared device {:?}",
                stream.device, expected
            ),
        });
    }
    Ok(())
}

fn ensure_grid_matches_stream(
    stream: &CapturedStream,
    grid: &RepeatGrid,
) -> Result<(), CaptureError> {
    if stream.session_id != grid.session_id {
        return Err(CaptureError::InvalidTimeline {
            detail: "stream session_id != grid session_id".to_string(),
        });
    }
    if stream.epoch_id != grid.epoch_id {
        return Err(CaptureError::InvalidTimeline {
            detail: "stream epoch_id != grid epoch_id".to_string(),
        });
    }
    if stream.graph_id != grid.graph_id {
        return Err(CaptureError::InvalidTimeline {
            detail: "stream graph_id != grid graph_id".to_string(),
        });
    }
    if stream.sample_rate_hz != grid.sample_rate_hz {
        return Err(CaptureError::UnsupportedSampleRate {
            actual: stream.sample_rate_hz,
            detail: format!("stream rate must equal grid rate {}", grid.sample_rate_hz),
        });
    }
    Ok(())
}

/// 完成声学采集：校验不混设备硬规则 -> 时基 -> 报告（raw sha256 绑定）。
///
/// - device 必须是 plan 声明的同一 UMIK（模型/序列号/校准文件/校准 sha/朝向/1ch 全部校验）；
/// - auxiliary 只能是另一个独立 MOTU 电学 reference 流，记录但不参与时基；
/// - capture_relative_path 是 job artifacts 目录内的相对路径（例如 capture.raw）。
pub fn finalize_acoustic_capture(
    umik: CapturedStream,
    device: &UmikDeviceConfig,
    grid: &RepeatGrid,
    plan: &ReferencePlan,
    onset_config: &OnsetConfig,
    capture_relative_path: &str,
    auxiliary: Option<CapturedStream>,
) -> Result<AcousticCaptureReport, CaptureError> {
    plan.validate()?;
    device.validate()?;
    if plan.purpose != CapturePurpose::AcousticRoom {
        return Err(CaptureError::MixedReferenceDevices {
            detail: "finalize_acoustic_capture requires purpose=acoustic".to_string(),
        });
    }
    let umik_device = plan
        .umik_device
        .as_deref()
        .ok_or(CaptureError::InvalidConfig {
            field: "plan.umik_device",
            detail: "acoustic plan requires a UMIK device".to_string(),
        })?;
    ensure_role(&umik, CaptureStreamRole::UmikAdc)?;
    ensure_device(&umik, umik_device)?;
    ensure_device(&umik, &device.device)?;
    if umik.sample_rate_hz != device.sample_rate_hz || umik.sample_format != device.sample_format {
        return Err(CaptureError::UnsupportedSampleRate {
            actual: umik.sample_rate_hz,
            detail: "capture stream rate/format does not match UMIK device declaration".to_string(),
        });
    }
    ensure_grid_matches_stream(&umik, grid)?;
    if let Some(auxiliary_stream) = &auxiliary {
        ensure_role(auxiliary_stream, CaptureStreamRole::MotuElectrical)?;
        if auxiliary_stream.device == umik.device {
            return Err(CaptureError::ReferenceNotIndependent {
                device: auxiliary_stream.device.clone(),
            });
        }
        let declared = plan
            .electrical
            .as_ref()
            .ok_or(CaptureError::InvalidConfig {
                field: "plan.electrical",
                detail: "auxiliary reference provided but not declared in plan".to_string(),
            })?;
        ensure_device(auxiliary_stream, &declared.device)?;
        if auxiliary_stream.sample_format != declared.sample_format {
            return Err(CaptureError::InvalidConfig {
                field: "plan.electrical.sample_format",
                detail: "auxiliary stream format does not match plan".to_string(),
            });
        }
        ensure_grid_matches_stream(auxiliary_stream, grid)?;
    }
    let capture_file = umik.capture_file(capture_relative_path)?;
    let seed = seed_from_stream(&umik)?;
    let noise_floor = compute_noise_floor(&umik.samples, seed, grid, umik.sample_rate_hz);
    let mut stream = umik.summary();
    stream.mic = Some(device.mic_report());
    stream.noise_floor_dbfs = noise_floor;
    ensure_device(&umik, &stream.device)?;
    let timebase = estimate_capture_timebase(&umik, grid, onset_config)?;
    let auxiliary_reference = match &auxiliary {
        Some(auxiliary_stream) => {
            let mut summary = auxiliary_stream.summary();
            summary.noise_floor_dbfs = None;
            Some(summary)
        }
        None => None,
    };
    Ok(AcousticCaptureReport {
        schema: UMIK_CAPTURE_SCHEMA,
        purpose: CapturePurpose::AcousticRoom,
        session_id: grid.session_id.clone(),
        plan: plan.clone(),
        stream,
        auxiliary_reference,
        timebase,
        capture_file,
        // T67 符号自测尚未执行：报告必须保持 INCOMPLETE，不得声称 ±3 dB（MICS.md）。
        mic_calibration: MicCalibrationVerification::not_run(),
    })
}

/// 完成电学环回采集：drift/相位基准都在 MOTU 电学 reference（UMIK 不参与）。
pub fn finalize_electrical_loopback(
    motu: CapturedStream,
    grid: &RepeatGrid,
    plan: &ReferencePlan,
    onset_config: &OnsetConfig,
    capture_relative_path: &str,
) -> Result<ElectricalLoopbackReport, CaptureError> {
    plan.validate()?;
    if plan.purpose != CapturePurpose::ElectricalLoopback {
        return Err(CaptureError::MixedReferenceDevices {
            detail: "finalize_electrical_loopback requires purpose=electrical_loopback".to_string(),
        });
    }
    let declared = plan
        .electrical
        .as_ref()
        .ok_or(CaptureError::InvalidConfig {
            field: "plan.electrical",
            detail: "electrical loopback requires an electrical reference channel".to_string(),
        })?;
    ensure_role(&motu, CaptureStreamRole::MotuElectrical)?;
    ensure_device(&motu, &declared.device)?;
    if motu.sample_format != declared.sample_format {
        return Err(CaptureError::InvalidConfig {
            field: "plan.electrical.sample_format",
            detail: "loopback stream format does not match plan".to_string(),
        });
    }
    if motu.reference_channel != Some(declared.input_channel) {
        return Err(CaptureError::InvalidConfig {
            field: "plan.electrical.input_channel",
            detail: "loopback stream channel does not match plan".to_string(),
        });
    }
    ensure_grid_matches_stream(&motu, grid)?;
    let capture_file = motu.capture_file(capture_relative_path)?;
    let timebase = estimate_capture_timebase(&motu, grid, onset_config)?;
    let mut stream = motu.summary();
    stream.noise_floor_dbfs = None;
    Ok(ElectricalLoopbackReport {
        schema: UMIK_CAPTURE_SCHEMA,
        purpose: CapturePurpose::ElectricalLoopback,
        session_id: grid.session_id.clone(),
        plan: plan.clone(),
        stream,
        timebase,
        capture_file,
    })
}

// ---------------------------------------------------------------------------
// 测试注册（leaf 私有测试自注册；integrator 不要再写 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod t62_tests {
    include!("../tests/T62/umik_capture.rs");
}

#[cfg(test)]
mod meas_fix_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/meas_fix/robust_snr.rs"
    ));
}
