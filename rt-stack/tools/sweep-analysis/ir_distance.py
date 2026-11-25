#!/usr/bin/env python3
"""IR 到直达延迟/距离几何（task-229 修复）—— 与 Rust crates/calibration/src/ir_distance.rs 同口径。

事故（2026-11-18）：sweep 验证把 ir.delay_ms_from_segment_start（IR 提取段内的相对峰值位置）
当成绝对直达延迟，再用 d = c*t 换算距离，得出 "MA1 0.08 ms 约 2.7 cm 近场/贴箱" 的错误结论。

口径（calibration-spec/MICS.md 第 2 节）：
- 相对量 relative_to_segment：段内峰值位置，只能段内对齐/相对比较，不得当绝对直达延迟/距离；
- 绝对量 absolute_from_t0：只有存在公共时基（同一次采集内的环回/参考通道扫频起点，或
  T61 render frame 到 T62 capture sample 的已知播放起点 t0）时才成立；
- 距离 d = c * (t_ir_onset - t0)，c 默认 343 m/s；未扣除 DAC/ADC 群延迟，除非 t0/环回包含同一链；
- 无公共时基 -> INVALID_NO_COMMON_TIMEBASE，distance_m = None（拒绝输出）；
- 房间/未知角色 < 0.5 m -> SUSPECT_TOO_CLOSE_FOR_ROOM_MIC（先怀疑时基/峰值，不得当近场证据）。

本模块只做纯计算，不读文件、不碰设备；自测见 selftest_ir_distance.py。
"""
import math

SOUND_SPEED_MPS = 343.0
MIN_ROOM_MIC_DISTANCE_M = 0.5
MIN_SPEED_OF_SOUND_MPS = 280.0
MAX_SPEED_OF_SOUND_MPS = 420.0

RELATIVE_TO_SEGMENT_FIELD = "relative_to_segment"
ABSOLUTE_FROM_T0_FIELD = "absolute_from_t0"
DISTANCE_FIELD = "distance"
LEGACY_AMBIGUOUS_FIELD = "delay_ms_from_segment_start"

MIC_ROLES = ("room_listening", "nearfield", "loopback_reference", "unknown")
TIMEBASE_SOURCES = ("loopback", "reference_channel", "known_playback_start")
SHORT_DISTANCE_ALLOWED_ROLES = ("nearfield", "loopback_reference")

STATUS_VALID = "VALID"
STATUS_SUSPECT = "SUSPECT_TOO_CLOSE_FOR_ROOM_MIC"
STATUS_NO_TIMEBASE = "INVALID_NO_COMMON_TIMEBASE"
STATUS_BAD_TIMEBASE = "INVALID_TIMEBASE"
STATUS_NEGATIVE = "INVALID_NEGATIVE_DELAY"
STATUS_BAD_SPEED = "INVALID_SPEED_OF_SOUND"


def sound_speed_mps(temperature_c):
    """温度修正声速 c 约等于 331.3 + 0.606*theta（m/s，theta 摄氏度）。"""
    return 331.3 + 0.606 * float(temperature_c)


def speed_of_sound_valid(speed_mps):
    return math.isfinite(speed_mps) and MIN_SPEED_OF_SOUND_MPS <= speed_mps <= MAX_SPEED_OF_SOUND_MPS


def recording_only_timebase():
    """单路录音时基：没有公共 t0，禁止输出距离。"""
    return {"source": None, "has_common_timebase": False, "t0_sample": None, "sample_rate_hz": None}


def known_timebase(source, t0_sample, sample_rate_hz):
    """构造公共时基；参数非法直接 ValueError（fail fast，不降级输出距离）。"""
    if source not in TIMEBASE_SOURCES:
        raise ValueError("timebase source must be one of %s, got %r" % (TIMEBASE_SOURCES, source))
    if not math.isfinite(sample_rate_hz) or sample_rate_hz <= 0:
        raise ValueError("sample_rate_hz must be finite and > 0")
    if not math.isfinite(t0_sample) or t0_sample < 0:
        raise ValueError("t0_sample must be finite and >= 0")
    return {
        "source": source,
        "has_common_timebase": True,
        "t0_sample": float(t0_sample),
        "sample_rate_hz": float(sample_rate_hz),
    }


def _json_float(value):
    if value is None or (isinstance(value, float) and not math.isfinite(value)):
        return None
    return float(value)


def relative_to_segment_ms(peak_index, sample_rate_hz, fractional_offset=None):
    if not math.isfinite(sample_rate_hz) or sample_rate_hz <= 0:
        raise ValueError("sample_rate_hz must be finite and > 0")
    offset = float(peak_index) + (0.0 if fractional_offset is None else float(fractional_offset))
    if fractional_offset is not None and abs(float(fractional_offset)) > 0.5 + 1e-12:
        raise ValueError("fractional_offset must be within [-0.5, 0.5]")
    return offset / sample_rate_hz * 1000.0


def find_direct_onset(ir, sample_rate_hz, segment_start_sample=None, noise_gate=0.0):
    """从 IR 找直达声峰值（全局 |ir| 最大 + 抛物线分数偏移）。

    返回 dict(peak_index, fractional_offset, segment_start_sample, sample_rate_hz) 或 None。
    注意：这是提取段内位置，不是绝对到达时刻（除非另有公共 t0）。
    """
    if not math.isfinite(sample_rate_hz) or sample_rate_hz <= 0:
        raise ValueError("sample_rate_hz must be finite and > 0")
    values = [abs(float(v)) for v in ir]
    if not values:
        return None
    peak_index = max(range(len(values)), key=lambda i: values[i])
    peak_abs = values[peak_index]
    if peak_abs <= 0.0 or peak_abs < noise_gate:
        return None
    fractional = None
    if 0 < peak_index < len(values) - 1:
        left, center, right = values[peak_index - 1], values[peak_index], values[peak_index + 1]
        denominator = left - 2.0 * center + right
        if math.isfinite(denominator) and abs(denominator) >= 1e-30:
            offset = 0.5 * (left - right) / denominator
            if math.isfinite(offset) and abs(offset) <= 0.5:
                fractional = offset
    return {
        "peak_index": int(peak_index),
        "fractional_offset": fractional,
        "segment_start_sample": None if segment_start_sample is None else float(segment_start_sample),
        "sample_rate_hz": float(sample_rate_hz),
    }


def estimate_direct_path(onset, timebase, mic_role="room_listening", speed_mps=SOUND_SPEED_MPS):
    """估计直达路径几何，返回确定性 dict（字段名与 Rust DirectPathEstimate::to_json 一致）。

    onset: find_direct_onset() 的返回，或手工 dict（peak_index/fractional_offset/
    segment_start_sample/sample_rate_hz）。timebase: recording_only_timebase() 或 known_timebase()。
    """
    if mic_role not in MIC_ROLES:
        raise ValueError("mic_role must be one of %s" % (MIC_ROLES,))
    peak_index = int(onset["peak_index"])
    sample_rate_hz = float(onset["sample_rate_hz"])
    segment_start = onset.get("segment_start_sample")
    fractional = onset.get("fractional_offset")
    offset_samples = peak_index + (0.0 if fractional is None else float(fractional))
    relative_ms = relative_to_segment_ms(peak_index, sample_rate_hz, fractional)
    result = {
        RELATIVE_TO_SEGMENT_FIELD: {
            "offset_samples": offset_samples,
            "delay_ms": relative_ms,
            "segment_start_sample": None if segment_start is None else float(segment_start),
        },
        "timebase": {
            "source": timebase.get("source"),
            "has_common_timebase": bool(timebase.get("has_common_timebase")),
            "t0_sample": timebase.get("t0_sample"),
            "sample_rate_hz": timebase.get("sample_rate_hz"),
        },
        ABSOLUTE_FROM_T0_FIELD: None,
        DISTANCE_FIELD: {"status": STATUS_NO_TIMEBASE, "distance_m": None, "mic_role": mic_role},
        "warnings": [],
    }
    if not result["timebase"]["has_common_timebase"]:
        result["warnings"].append(
            "no_common_timebase: segment_start is not playback t0; relative offset must not be converted to distance")
        return result
    t0_sample = timebase.get("t0_sample")
    if t0_sample is None or not math.isfinite(t0_sample) or t0_sample < 0:
        result["distance"]["status"] = STATUS_BAD_TIMEBASE
        result["warnings"].append("t0_missing: common timebase requires a finite t0_sample")
        return result
    timebase_rate = timebase.get("sample_rate_hz")
    if timebase_rate is None or not math.isfinite(timebase_rate) or timebase_rate <= 0:
        result["distance"]["status"] = STATUS_BAD_TIMEBASE
        result["warnings"].append("timebase_sample_rate_unknown: t0 must carry its capture sample rate")
        return result
    if abs(float(timebase_rate) - sample_rate_hz) > 1e-6 * sample_rate_hz:
        result["distance"]["status"] = STATUS_BAD_TIMEBASE
        result["warnings"].append(
            "timebase_sample_rate_mismatch: t0 and IR onset must share one capture domain")
        return result
    if segment_start is None:
        result["distance"]["status"] = STATUS_BAD_TIMEBASE
        result["warnings"].append("segment_start_unknown: capture-domain IR onset cannot be located")
        return result
    onset_capture = float(segment_start) + offset_samples
    direct_seconds = (onset_capture - float(t0_sample)) / sample_rate_hz
    result[ABSOLUTE_FROM_T0_FIELD] = direct_seconds * 1000.0
    if direct_seconds < 0.0:
        result["distance"]["status"] = STATUS_NEGATIVE
        result["warnings"].append("negative_direct_delay: IR onset precedes t0")
        return result
    if not speed_of_sound_valid(float(speed_mps)):
        result["distance"]["status"] = STATUS_BAD_SPEED
        result["warnings"].append("speed_of_sound_out_of_range: expected ~343 m/s")
        return result
    distance_m = float(speed_mps) * direct_seconds
    result["distance"]["distance_m"] = distance_m
    if mic_role not in SHORT_DISTANCE_ALLOWED_ROLES and distance_m < MIN_ROOM_MIC_DISTANCE_M:
        result["distance"]["status"] = STATUS_SUSPECT
        result["warnings"].append(
            "distance_below_0.5m_for_room_mic: suspect timebase/peak selection, not a near-field proof")
        return result
    result["distance"]["status"] = STATUS_VALID
    return result


def distance_usable(estimate):
    return estimate["distance"]["status"] == STATUS_VALID