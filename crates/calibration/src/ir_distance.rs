//! [NONRT] IR 到直达延迟/距离几何（task-229；时基语义与 sanity 门） - T60/T61/T62/T63/T67 消费。
//!
//! 起因（2026-11-18 sweep 验证事故）：task-227 的 sweep README/JSON 把
//! ir.delay_ms_from_segment_start（IR 提取段内的相对峰值位置）当成绝对直达延迟，
//! 再用 d = c*t 换算距离，得出 "MA1 直达延迟 0.08 ms 约 2.7 cm（近场/贴箱）" 的错误结论。
//! 真实约束：单路录音的 t=0 只是采集启动时刻，不是播放起点；IR 提取段起点（反卷积
//! 对齐点）也不是已知的声学发射时刻。没有公共时基时，段内偏移只能用于段内对齐/相对比较。
//!
//! 正确口径（calibration-spec/MICS.md 第 2 节 "IR/延迟/距离唯一口径"）：
//! - 相对量：relative_to_segment = (peak_index + fractional_offset) / fs，只表示直达声在
//!   IR 提取段内的位置，不得当绝对直达延迟，更不得换算距离。
//! - 绝对量：t_direct = (t_ir_onset - t0) / fs，仅当存在公共时基（同一次采集内的
//!   环回/参考通道扫频起点，或 T61 render frame 到 T62 capture sample 的已知播放起点
//!   t0_sample）时成立；t_ir_onset 与 t0 必须同采样率/同采集域。
//! - 距离：d = c * t_direct，c 默认 343 m/s（约 20 摄氏度空气），可用
//!   sound_speed_mps() 按温度修正；分辨率 = c/fs（48 kHz 时 7.15 mm/样本）。未扣除
//!   DAC/ADC/抗混叠群延迟，除非 t0/环回已包含同一测量链；分数延迟来自峰值抛物线插值，
//!   group delay 口径必须与 t0 一致。
//! - 无公共时基 -> DistanceStatus::InvalidNoCommonTimebase，distance_m = None，拒绝输出。
//! - 房间/听音位麦（或未知角色）若算出 < 0.5 m -> DistanceStatus::SuspectTooCloseForRoomMic
//!   （先怀疑时基/峰值选错，绝不直接写 "近场/贴箱"）。近场/环回参考角色不设该下限。
//!
//! 写域：task-229（rt-stack/crates/calibration）；本模块不打开设备、不读文件，纯离线可测。
//! 契约：calibration-spec/MICS.md 第 2 节（Lead 2026-11-18 口径）+ rt-stack/spec/mimo-t6.md T6 几何口径。

use std::fmt;

/// 20 摄氏度干燥空气声速近似（m/s）。项目统一取 343 m/s（MICS.md 第 2 节）。
pub const SOUND_SPEED_MPS: f64 = 343.0;

/// 房间/听音位麦的最小合理直达距离（m）；低于此值先怀疑时基或峰值选错。
pub const MIN_ROOM_MIC_DISTANCE_M: f64 = 0.5;

/// 声速合理范围（m/s）：覆盖 0..50 摄氏度并挡住单位错误（0.343 / 343000 / 343e-3）。
pub const MIN_SPEED_OF_SOUND_MPS: f64 = 280.0;
/// 声速合理范围上限（m/s）。
pub const MAX_SPEED_OF_SOUND_MPS: f64 = 420.0;

/// 机器可读字段名：IR 提取段内相对偏移（只能段内对齐/相对比较）。
pub const RELATIVE_TO_SEGMENT_FIELD: &str = "relative_to_segment";
/// 机器可读字段名：相对公共时基 t0 的绝对直达延迟（仅公共时基时出现）。
pub const ABSOLUTE_FROM_T0_FIELD: &str = "absolute_from_t0";
/// 机器可读字段名：距离结论（status + distance_m）。
pub const DISTANCE_FIELD: &str = "distance";
/// 历史歧义字段名：曾把段内偏移当绝对直达延迟；新代码禁止再用作绝对量/距离来源。
pub const LEGACY_AMBIGUOUS_FIELD: &str = "delay_ms_from_segment_start";

/// 麦克风角色：决定 "距离过近" 是否可疑（房间麦 <0.5 m 必须先怀疑时基/峰值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicRole {
    /// 听音位/房间麦（MA1、UMIK 在听音位等）。
    RoomListening,
    /// 单只音箱近场/轴上麦（允许 <0.5 m）。
    Nearfield,
    /// 环回/参考通道（电学回采，允许近零距离）。
    LoopbackReference,
    /// 未声明角色：保守按房间麦处理。
    Unknown,
}

impl MicRole {
    /// 机器名（JSON）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoomListening => "room_listening",
            Self::Nearfield => "nearfield",
            Self::LoopbackReference => "loopback_reference",
            Self::Unknown => "unknown",
        }
    }

    /// 该角色是否允许 <0.5 m 的合理小距离（近场/环回）。
    pub const fn allows_short_distance(self) -> bool {
        matches!(self, Self::Nearfield | Self::LoopbackReference)
    }
}

/// 公共时基来源（必须是能给出 "播放起点/参考直达" 在同一采集域采样的来源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimebaseSource {
    /// 同一次采集内记录的电学环回通道扫频起点（T1 partial；DAC 到 ADC 链路）。
    Loopback,
    /// 同一次采集内的参考通道/参考麦（已知几何或已知直达）。
    ReferenceChannel,
    /// 已知播放起点：T61 render frame 与 T62 capture sample 映射给出的 t0。
    KnownPlaybackStart,
}

impl TimebaseSource {
    /// 机器名（JSON）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::ReferenceChannel => "reference_channel",
            Self::KnownPlaybackStart => "known_playback_start",
        }
    }
}

/// 是否存在公共时基。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommonTimebase {
    /// 只有采集流自身的 t=0（录音启动时刻）：禁止推算绝对延迟/距离。
    RecordingOnly,
    /// 有公共时基：t0_sample 为播放起点/环回起点/参考直达在同一采集流中的采样位置。
    Known {
        /// 来源。
        source: TimebaseSource,
        /// t0 采样位置（与 IR onset 同一采集域）。
        t0_sample: f64,
        /// 采集域采样率（必须与 IR 分析采样率一致）。
        sample_rate_hz: f64,
    },
}

impl CommonTimebase {
    /// 构造带公共时基的描述并校验参数（有限、非负、采样率 > 0）。
    pub fn known(
        source: TimebaseSource,
        t0_sample: f64,
        sample_rate_hz: f64,
    ) -> Result<Self, GeometryError> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(GeometryError::InvalidSampleRate);
        }
        if !t0_sample.is_finite() || t0_sample < 0.0 {
            return Err(GeometryError::InvalidT0);
        }
        Ok(Self::Known {
            source,
            t0_sample,
            sample_rate_hz,
        })
    }

    /// 是否有公共时基。
    pub const fn has_common_timebase(self) -> bool {
        matches!(self, Self::Known { .. })
    }

    /// 时基来源（无公共时基为 None）。
    pub const fn source(self) -> Option<TimebaseSource> {
        match self {
            Self::RecordingOnly => None,
            Self::Known { source, .. } => Some(source),
        }
    }
}

/// IR 提取段内的直达声位置（峰值 + 可选分数延迟）。
///
/// peak_index / fractional_offset 相对提取段起点；segment_start_sample 若可追溯，
/// 则 capture_sample() = segment_start + peak_index + fractional_offset。只有同时给出
/// 公共时基 t0 才可能得到绝对延迟/距离。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IrDirectOnset {
    /// IR 提取段内的整数峰索引。
    pub peak_index: usize,
    /// 峰值抛物线插值的小数偏移（-0.5..=0.5）；None = 只有整数分辨率。
    pub fractional_offset: Option<f64>,
    /// IR 提取段起点在采集流中的采样位置（可追溯时；反卷积对齐点）。
    pub segment_start_sample: Option<f64>,
    /// IR 分析采样率（Hz）。
    pub sample_rate_hz: f64,
}

impl IrDirectOnset {
    /// 构造并校验（采样率 > 0、分数偏移有限且在 [-0.5, 0.5]、段起点有限非负）。
    pub fn new(
        peak_index: usize,
        fractional_offset: Option<f64>,
        segment_start_sample: Option<f64>,
        sample_rate_hz: f64,
    ) -> Result<Self, GeometryError> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(GeometryError::InvalidSampleRate);
        }
        if let Some(offset) = fractional_offset {
            if !offset.is_finite() || offset.abs() > 0.5 + 1e-12 {
                return Err(GeometryError::InvalidFractionalOffset);
            }
        }
        if let Some(start) = segment_start_sample {
            if !start.is_finite() || start < 0.0 {
                return Err(GeometryError::InvalidSegmentStart);
            }
        }
        Ok(Self {
            peak_index,
            fractional_offset,
            segment_start_sample,
            sample_rate_hz,
        })
    }

    /// 段内偏移（样本，含分数部分）。
    pub fn offset_in_segment_samples(self) -> f64 {
        self.peak_index as f64 + self.fractional_offset.unwrap_or(0.0)
    }

    /// 段内相对偏移（ms）。只能用于段内对齐/相对比较，不是绝对直达延迟。
    pub fn relative_to_segment_ms(self) -> f64 {
        self.offset_in_segment_samples() / self.sample_rate_hz * 1000.0
    }

    /// 直达声在采集流中的采样位置；段起点不可追溯时为 None。
    pub fn capture_sample(self) -> Option<f64> {
        self.segment_start_sample
            .map(|start| start + self.offset_in_segment_samples())
    }
}

/// 距离/延迟结论状态（机器名进 JSON）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceStatus {
    /// 公共时基 + 参数 + sanity 全通过；distance_m 可用。
    Valid,
    /// 公共时基存在但角色是房间麦/未知且距离 <0.5 m：先把距离当 "时基/峰值选错" 信号。
    SuspectTooCloseForRoomMic,
    /// 无公共时基：拒绝输出绝对延迟/距离（本次事故根因）。
    InvalidNoCommonTimebase,
    /// 时基参数/采样率不一致/段起点不可追溯等。
    InvalidTimebase,
    /// t_ir_onset < t0（负延迟）。
    InvalidNegativeDelay,
    /// 声速超出合理范围（多半是单位错误）。
    InvalidSpeedOfSound,
}

impl DistanceStatus {
    /// 机器名（JSON）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "VALID",
            Self::SuspectTooCloseForRoomMic => "SUSPECT_TOO_CLOSE_FOR_ROOM_MIC",
            Self::InvalidNoCommonTimebase => "INVALID_NO_COMMON_TIMEBASE",
            Self::InvalidTimebase => "INVALID_TIMEBASE",
            Self::InvalidNegativeDelay => "INVALID_NEGATIVE_DELAY",
            Self::InvalidSpeedOfSound => "INVALID_SPEED_OF_SOUND",
        }
    }

    /// 是否可用于任何下游结论（只有 Valid 可以）。
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Valid)
    }
}

/// 直达路径几何估计。
#[derive(Debug, Clone, PartialEq)]
pub struct DirectPathEstimate {
    /// 结论状态。
    pub status: DistanceStatus,
    /// 段内相对偏移（ms）：任何情况都可算，但不是绝对直达延迟。
    pub relative_to_segment_ms: f64,
    /// 段内偏移（样本，含分数部分）。
    pub offset_in_segment_samples: f64,
    /// IR 提取段起点（采集样本；未知为 None）。
    pub segment_start_sample: Option<f64>,
    /// 绝对直达延迟（ms）：仅公共时基时 Some。
    pub absolute_from_t0_ms: Option<f64>,
    /// 距离（m）：仅公共时基时 Some（Suspect 也保留数值供诊断，但不可用）。
    pub distance_m: Option<f64>,
    /// 时基来源（无公共时基为 None）。
    pub timebase_source: Option<TimebaseSource>,
    /// 麦克风角色。
    pub mic_role: MicRole,
    /// 使用/拒绝理由（稳定 ASCII 短语，进 JSON）。
    pub warnings: Vec<&'static str>,
}

impl DirectPathEstimate {
    /// 是否可用于下游（只有 Valid）。
    pub fn usable(&self) -> bool {
        self.status.is_usable()
    }

    /// 确定性 JSON（无第三方依赖）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push('{');
        out.push('"');
        out.push_str(RELATIVE_TO_SEGMENT_FIELD);
        out.push_str("\":{");
        out.push_str(&format!(
            "\"offset_samples\":{},\"delay_ms\":{},\"segment_start_sample\":",
            json_f64(self.offset_in_segment_samples),
            json_f64(self.relative_to_segment_ms)
        ));
        match self.segment_start_sample {
            Some(start) => out.push_str(&json_f64(start)),
            None => out.push_str("null"),
        }
        out.push_str("},\"timebase\":{\"source\":");
        match self.timebase_source {
            Some(source) => push_json_str(&mut out, source.as_str()),
            None => out.push_str("null"),
        }
        out.push_str(&format!(
            ",\"has_common_timebase\":{}}}",
            if self.timebase_source.is_some() {
                "true"
            } else {
                "false"
            }
        ));
        out.push('"');
        out.push(',');
        out.push('"');
        out.push_str(ABSOLUTE_FROM_T0_FIELD);
        out.push_str("\":");
        match self.absolute_from_t0_ms {
            Some(ms) => out.push_str(&json_f64(ms)),
            None => out.push_str("null"),
        }
        out.push_str(",\"");
        out.push_str(DISTANCE_FIELD);
        out.push_str("\":{\"status\":");
        push_json_str(&mut out, self.status.as_str());
        out.push_str(",\"distance_m\":");
        match self.distance_m {
            Some(m) => out.push_str(&json_f64(m)),
            None => out.push_str("null"),
        }
        out.push_str(",\"mic_role\":");
        push_json_str(&mut out, self.mic_role.as_str());
        out.push_str("},\"warnings\":[");
        for (index, warning) in self.warnings.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_str(&mut out, warning);
        }
        out.push_str("]}");
        out
    }
}

/// 几何/单位错误（参数级；结论级用 DistanceStatus）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// 采样率非有限正数。
    InvalidSampleRate,
    /// 分数偏移超出 [-0.5, 0.5]。
    InvalidFractionalOffset,
    /// t0 非有限/为负。
    InvalidT0,
    /// 段起点非有限/为负。
    InvalidSegmentStart,
    /// 声速超出合理范围。
    InvalidSpeedOfSound,
    /// IR onset 与 t0 采样率不一致（不同采集域/重采样未声明）。
    SampleRateMismatch,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSampleRate => "sample rate must be finite and > 0",
            Self::InvalidFractionalOffset => {
                "fractional offset must be finite and within [-0.5, 0.5]"
            }
            Self::InvalidT0 => "t0 sample must be finite and >= 0",
            Self::InvalidSegmentStart => "segment start sample must be finite and >= 0",
            Self::InvalidSpeedOfSound => "speed of sound outside 280..420 m/s (unit error?)",
            Self::SampleRateMismatch => "IR onset sample rate differs from t0 sample rate",
        })
    }
}

impl std::error::Error for GeometryError {}

/// 温度修正声速：c 约等于 331.3 + 0.606*theta（m/s，theta=摄氏度，0..50 度干空气近似）。
pub fn sound_speed_mps(temperature_c: f64) -> f64 {
    331.3 + 0.606 * temperature_c
}

/// 声速是否在合理范围（挡住 0.343 / 343000 等单位错误）。
pub fn speed_of_sound_valid(speed_mps: f64) -> bool {
    speed_mps.is_finite() && (MIN_SPEED_OF_SOUND_MPS..=MAX_SPEED_OF_SOUND_MPS).contains(&speed_mps)
}

/// 从 IR 提取直达声峰值：全局 |ir| 最大点 + 抛物线插值分数偏移。
///
/// noise_gate 为绝对门限（如 1e-6）；全局峰低于该门限时返回 None（IR 无有效直达声）。
/// segment_start_sample 为提取段起点（可追溯时）。
pub fn find_direct_onset(
    ir: &[f64],
    noise_gate: f64,
    sample_rate_hz: f64,
    segment_start_sample: Option<f64>,
) -> Result<Option<IrDirectOnset>, GeometryError> {
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(GeometryError::InvalidSampleRate);
    }
    if ir.is_empty() {
        return Ok(None);
    }
    let mut peak_index = 0usize;
    let mut peak_abs = 0.0f64;
    for (index, value) in ir.iter().enumerate() {
        let abs = value.abs();
        if abs.is_finite() && abs > peak_abs {
            peak_abs = abs;
            peak_index = index;
        }
    }
    if peak_abs <= 0.0 || peak_abs < noise_gate {
        return Ok(None);
    }
    let fractional_offset = parabolic_fractional_offset(ir, peak_index);
    Ok(Some(IrDirectOnset {
        peak_index,
        fractional_offset,
        segment_start_sample,
        sample_rate_hz,
    }))
}

/// 三点抛物线插值的小数峰偏移（-0.5..=0.5）；边界/退化返回 None。
fn parabolic_fractional_offset(ir: &[f64], peak_index: usize) -> Option<f64> {
    if peak_index == 0 || peak_index + 1 >= ir.len() {
        return None;
    }
    let left = ir[peak_index - 1].abs();
    let center = ir[peak_index].abs();
    let right = ir[peak_index + 1].abs();
    let denominator = left - 2.0 * center + right;
    if !denominator.is_finite() || denominator.abs() < 1e-30 {
        return None;
    }
    let offset = 0.5 * (left - right) / denominator;
    if offset.is_finite() && offset.abs() <= 0.5 {
        Some(offset)
    } else {
        None
    }
}

/// 估计直达路径几何（默认 343 m/s 声速）。
pub fn estimate_direct_path(
    onset: &IrDirectOnset,
    timebase: CommonTimebase,
    mic_role: MicRole,
) -> DirectPathEstimate {
    estimate_direct_path_with_speed(onset, timebase, mic_role, SOUND_SPEED_MPS)
}

/// 估计直达路径几何（显式声速，便于温度修正/单位测试）。
///
/// 规则：
/// 1. CommonTimebase::RecordingOnly -> INVALID_NO_COMMON_TIMEBASE，绝不输出距离；
/// 2. 采样率不一致/段起点不可追溯 -> INVALID_TIMEBASE；
/// 3. t_ir_onset < t0 -> INVALID_NEGATIVE_DELAY；
/// 4. 房间/未知角色且 d < 0.5 m -> SUSPECT_TOO_CLOSE_FOR_ROOM_MIC（保留数值供诊断）；
/// 5. 其余 -> VALID。
pub fn estimate_direct_path_with_speed(
    onset: &IrDirectOnset,
    timebase: CommonTimebase,
    mic_role: MicRole,
    speed_mps: f64,
) -> DirectPathEstimate {
    let mut estimate = DirectPathEstimate {
        status: DistanceStatus::InvalidNoCommonTimebase,
        relative_to_segment_ms: onset.relative_to_segment_ms(),
        offset_in_segment_samples: onset.offset_in_segment_samples(),
        segment_start_sample: onset.segment_start_sample,
        absolute_from_t0_ms: None,
        distance_m: None,
        timebase_source: timebase.source(),
        mic_role,
        warnings: Vec::new(),
    };
    let (source, t0_sample, timebase_rate) = match timebase {
        CommonTimebase::RecordingOnly => {
            estimate.warnings.push(
                "no_common_timebase: segment_start is not playback t0; relative offset must not be converted to distance",
            );
            return estimate;
        }
        CommonTimebase::Known {
            source,
            t0_sample,
            sample_rate_hz,
        } => (source, t0_sample, sample_rate_hz),
    };
    estimate.timebase_source = Some(source);
    if !(timebase_rate.is_finite() && timebase_rate > 0.0)
        || (timebase_rate - onset.sample_rate_hz).abs() > 1e-6 * onset.sample_rate_hz
    {
        estimate.status = DistanceStatus::InvalidTimebase;
        estimate
            .warnings
            .push("timebase_sample_rate_mismatch: t0 and IR onset must share one capture domain");
        return estimate;
    }
    let onset_capture = match onset.capture_sample() {
        Some(sample) => sample,
        None => {
            estimate.status = DistanceStatus::InvalidTimebase;
            estimate
                .warnings
                .push("segment_start_unknown: capture-domain IR onset cannot be located");
            return estimate;
        }
    };
    let direct_seconds = (onset_capture - t0_sample) / timebase_rate;
    estimate.absolute_from_t0_ms = Some(direct_seconds * 1000.0);
    if direct_seconds < 0.0 {
        estimate.status = DistanceStatus::InvalidNegativeDelay;
        estimate
            .warnings
            .push("negative_direct_delay: IR onset precedes t0");
        return estimate;
    }
    if !speed_of_sound_valid(speed_mps) {
        estimate.status = DistanceStatus::InvalidSpeedOfSound;
        estimate
            .warnings
            .push("speed_of_sound_out_of_range: expected ~343 m/s");
        return estimate;
    }
    let distance_m = speed_mps * direct_seconds;
    estimate.distance_m = Some(distance_m);
    if !mic_role.allows_short_distance() && distance_m < MIN_ROOM_MIC_DISTANCE_M {
        estimate.status = DistanceStatus::SuspectTooCloseForRoomMic;
        estimate.warnings.push(
            "distance_below_0.5m_for_room_mic: suspect timebase/peak selection, not a near-field proof",
        );
        return estimate;
    }
    estimate.status = DistanceStatus::Valid;
    estimate
}

fn json_f64(value: f64) -> String {
    if value.is_finite() {
        let mut text = format!("{value}");
        if !text.contains('.') && !text.contains('e') && !text.contains('E') {
            text.push_str(".0");
        }
        text
    } else {
        "null".to_string()
    }
}

fn push_json_str(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    /// 合成 IR：直达声脉冲（可带分数延迟/极性）+ 两条衰减反射。
    fn synthetic_ir(direct_sample: f64, len: usize, polarity: f64) -> Vec<f64> {
        let mut ir = vec![0.0; len];
        let integer = direct_sample.floor() as usize;
        let fraction = direct_sample - integer as f64;
        if integer < len {
            ir[integer] = polarity * (1.0 - fraction);
        }
        if fraction > 0.0 && integer + 1 < len {
            ir[integer + 1] = polarity * fraction;
        }
        for (delay_ms, gain) in [(3.0f64, 0.30f64), (7.0, 0.15)] {
            let index = integer + (delay_ms / 1000.0 * FS) as usize;
            if index < len {
                ir[index] += polarity * gain;
            }
        }
        ir
    }

    #[test]
    fn t229_synthetic_ir_with_common_timebase_yields_distance() {
        let ir = synthetic_ir(96.0, 4096, 1.0);
        let onset = find_direct_onset(&ir, 0.0, FS, Some(0.0))
            .expect("detect onset")
            .expect("peak present");
        assert_eq!(onset.peak_index, 96);
        let timebase = CommonTimebase::known(TimebaseSource::Loopback, 0.0, FS).expect("timebase");
        let estimate = estimate_direct_path(&onset, timebase, MicRole::RoomListening);
        assert!(estimate.usable(), "status={:?}", estimate.status);
        assert_eq!(estimate.status, DistanceStatus::Valid);
        let distance = estimate.distance_m.expect("distance");
        assert!(
            (distance - 343.0 * 0.002).abs() < 1e-9,
            "distance={distance}"
        );
        assert!((estimate.absolute_from_t0_ms.expect("abs") - 2.0).abs() < 1e-9);
        assert!((estimate.relative_to_segment_ms - 2.0).abs() < 1e-9);
        assert_eq!(estimate.timebase_source, Some(TimebaseSource::Loopback));
    }

    #[test]
    fn t229_synthetic_ir_without_t0_refuses_distance() {
        let ir = synthetic_ir(96.0, 4096, 1.0);
        let onset = find_direct_onset(&ir, 0.0, FS, Some(12_345.0))
            .expect("detect onset")
            .expect("peak present");
        let estimate = estimate_direct_path(
            &onset,
            CommonTimebase::RecordingOnly,
            MicRole::RoomListening,
        );
        assert_eq!(estimate.status, DistanceStatus::InvalidNoCommonTimebase);
        assert!(!estimate.usable());
        assert_eq!(estimate.distance_m, None);
        assert_eq!(estimate.absolute_from_t0_ms, None);
        assert_eq!(estimate.timebase_source, None);
        assert!((estimate.relative_to_segment_ms - 2.0).abs() < 1e-9);
        assert!(estimate
            .warnings
            .iter()
            .any(|warning| warning.contains("no_common_timebase")));
        let json = estimate.to_json();
        assert!(
            json.contains("\"has_common_timebase\":false"),
            "json={json}"
        );
        assert!(json.contains("\"distance_m\":null"), "json={json}");
        assert!(
            !json.contains(LEGACY_AMBIGUOUS_FIELD),
            "legacy ambiguous field must not reappear: {json}"
        );
    }

    #[test]
    fn t229_room_mic_zero_delay_flags_suspect_not_valid() {
        let ir = synthetic_ir(4.0, 4096, 1.0);
        let onset = find_direct_onset(&ir, 0.0, FS, Some(0.0))
            .expect("detect onset")
            .expect("peak present");
        let timebase =
            CommonTimebase::known(TimebaseSource::KnownPlaybackStart, 0.0, FS).expect("timebase");
        let estimate = estimate_direct_path(&onset, timebase, MicRole::RoomListening);
        assert_eq!(estimate.status, DistanceStatus::SuspectTooCloseForRoomMic);
        assert!(!estimate.usable());
        let distance = estimate.distance_m.expect("diagnostic distance");
        assert!(
            (distance - 343.0 * 4.0 / FS).abs() < 1e-9,
            "distance={distance}"
        );
        assert!(estimate
            .warnings
            .iter()
            .any(|warning| warning.contains("0.5m")));
        let nearfield = estimate_direct_path(&onset, timebase, MicRole::Nearfield);
        assert_eq!(nearfield.status, DistanceStatus::Valid);
        let loopback = estimate_direct_path(&onset, timebase, MicRole::LoopbackReference);
        assert_eq!(loopback.status, DistanceStatus::Valid);
        let unknown = estimate_direct_path(&onset, timebase, MicRole::Unknown);
        assert_eq!(unknown.status, DistanceStatus::SuspectTooCloseForRoomMic);
    }

    #[test]
    fn t229_fractional_delay_and_segment_start_offset() {
        let onset = IrDirectOnset::new(96, Some(0.4), Some(1000.0), FS).expect("onset");
        let timebase = CommonTimebase::known(TimebaseSource::KnownPlaybackStart, 1000.0, FS)
            .expect("timebase");
        let estimate = estimate_direct_path(&onset, timebase, MicRole::RoomListening);
        assert_eq!(estimate.status, DistanceStatus::Valid);
        let expected_ms = 96.4 / FS * 1000.0;
        assert!((estimate.absolute_from_t0_ms.expect("abs") - expected_ms).abs() < 1e-12);
        let expected_distance = 343.0 * 96.4 / FS;
        assert!((estimate.distance_m.expect("distance") - expected_distance).abs() < 1e-12);
        let speed_20c = sound_speed_mps(20.0);
        assert!((speed_20c - 343.42).abs() < 1e-9);
        let warmed =
            estimate_direct_path_with_speed(&onset, timebase, MicRole::RoomListening, speed_20c);
        assert!((warmed.distance_m.expect("distance") - speed_20c * 96.4 / FS).abs() < 1e-12);
    }

    #[test]
    fn t229_rejects_bad_timebase_and_negative_delay() {
        let onset = IrDirectOnset::new(96, None, Some(1000.0), FS).expect("onset");
        let mismatch =
            CommonTimebase::known(TimebaseSource::Loopback, 0.0, 44_100.0).expect("timebase");
        assert_eq!(
            estimate_direct_path(&onset, mismatch, MicRole::RoomListening).status,
            DistanceStatus::InvalidTimebase
        );
        let untraceable = IrDirectOnset::new(96, None, None, FS).expect("onset");
        let timebase = CommonTimebase::known(TimebaseSource::Loopback, 0.0, FS).expect("timebase");
        assert_eq!(
            estimate_direct_path(&untraceable, timebase, MicRole::RoomListening).status,
            DistanceStatus::InvalidTimebase
        );
        let late_t0 =
            CommonTimebase::known(TimebaseSource::Loopback, 2000.0, FS).expect("timebase");
        let negative = estimate_direct_path(&onset, late_t0, MicRole::RoomListening);
        assert_eq!(negative.status, DistanceStatus::InvalidNegativeDelay);
        assert_eq!(negative.distance_m, None);
    }

    #[test]
    fn t229_rejects_unit_error_speed_of_sound() {
        let onset = IrDirectOnset::new(96, None, Some(0.0), FS).expect("onset");
        let timebase = CommonTimebase::known(TimebaseSource::Loopback, 0.0, FS).expect("timebase");
        for bad in [0.343f64, 343_000.0, f64::NAN, -343.0] {
            let estimate =
                estimate_direct_path_with_speed(&onset, timebase, MicRole::RoomListening, bad);
            assert_eq!(
                estimate.status,
                DistanceStatus::InvalidSpeedOfSound,
                "speed={bad}"
            );
            assert_eq!(estimate.distance_m, None);
        }
    }

    #[test]
    fn t229_constructors_validate_inputs() {
        assert_eq!(
            IrDirectOnset::new(0, Some(0.51), Some(0.0), FS).unwrap_err(),
            GeometryError::InvalidFractionalOffset
        );
        assert_eq!(
            IrDirectOnset::new(0, None, Some(-1.0), FS).unwrap_err(),
            GeometryError::InvalidSegmentStart
        );
        assert_eq!(
            IrDirectOnset::new(0, None, Some(0.0), 0.0).unwrap_err(),
            GeometryError::InvalidSampleRate
        );
        assert_eq!(
            CommonTimebase::known(TimebaseSource::Loopback, -1.0, FS).unwrap_err(),
            GeometryError::InvalidT0
        );
        assert_eq!(CommonTimebase::RecordingOnly.source(), None);
        assert!(!CommonTimebase::RecordingOnly.has_common_timebase());
        assert!(CommonTimebase::known(TimebaseSource::Loopback, 0.0, FS)
            .expect("timebase")
            .has_common_timebase());
    }

    #[test]
    fn t229_find_direct_onset_handles_empty_and_bad_ir() {
        assert_eq!(
            find_direct_onset(&[], 0.0, FS, Some(0.0)).expect("ok"),
            None
        );
        assert_eq!(
            find_direct_onset(&[0.0; 128], 0.0, FS, Some(0.0)).expect("ok"),
            None
        );
        assert_eq!(
            find_direct_onset(&[1.0; 8], 0.0, 0.0, None).unwrap_err(),
            GeometryError::InvalidSampleRate
        );
        let ir = [0.0, 0.25, 1.0, 0.5, 0.0];
        let onset = find_direct_onset(&ir, 0.0, FS, Some(10.0))
            .expect("ok")
            .expect("peak");
        assert_eq!(onset.peak_index, 2);
        // 公式：0.5*(left-right)/(left-2*center+right) = 0.5*(0.25-0.5)/(0.25-2.0+0.5) = 0.1。
        assert!((onset.fractional_offset.expect("frac") - 0.1).abs() < 1e-12);
        assert!((onset.capture_sample().expect("capture") - (10.0 + 2.1)).abs() < 1e-9);
    }
}
