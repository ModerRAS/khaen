#!/usr/bin/env python3
"""合成 IR 单测：task-229 IR 到距离口径（正确 t0 -> 距离；无 t0 -> 拒绝）。

覆盖：已知 delay + 公共时基 -> 距离；无公共时基 -> INVALID 且不输出距离；
房间麦 <0.5 m -> SUSPECT；近场/环回角色放行；分数延迟；采样率不一致；负延迟；
声速单位错误；参数校验；find_direct_onset 上限/退化；JSON 字段语义（无旧歧义字段）。
退出码 0 = 全部 PASS，1 = 有 FAIL（不能当 PASS 用）。
"""
import json
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ir_distance as geo  # noqa: E402

PASS = 0
FAIL = 0
FAILURES = []


def check(name, condition, detail=""):
    global PASS, FAIL
    if condition:
        PASS += 1
        print("PASS  %s" % name)
    else:
        FAIL += 1
        FAILURES.append(name)
        print("FAIL  %s  %s" % (name, detail))


def close(a, b, tol=1e-12):
    return a is not None and b is not None and math.isfinite(a) and abs(a - b) <= tol


def synthetic_ir(direct_sample, length=4096, polarity=1.0):
    """合成 IR：直达声脉冲（可带分数延迟）+ 两条衰减反射。"""
    integer = int(math.floor(direct_sample))
    fraction = direct_sample - integer
    ir = [0.0] * length
    if integer < length:
        ir[integer] = polarity * (1.0 - fraction)
    if fraction > 0.0 and integer + 1 < length:
        ir[integer + 1] = polarity * fraction
    for delay_ms, gain in ((3.0, 0.30), (7.0, 0.15)):
        index = integer + int(delay_ms / 1000.0 * 48000.0)
        if index < length:
            ir[index] += polarity * gain
    return ir


FS = 48000.0

def main():
    # 1) 已知 delay 96 样本 + 公共 t0(=0) -> 2 ms -> 0.686 m
    ir = synthetic_ir(96.0)
    onset = geo.find_direct_onset(ir, FS, segment_start_sample=0.0)
    check("find_direct_onset_peak_index", onset is not None and onset["peak_index"] == 96, repr(onset))
    timebase = geo.known_timebase("loopback", 0.0, FS)
    est = geo.estimate_direct_path(onset, timebase, mic_role="room_listening")
    check("known_t0_status_VALID", est["distance"]["status"] == geo.STATUS_VALID, est["distance"]["status"])
    check("known_t0_distance_0.686m", close(est["distance"]["distance_m"], 343.0 * 0.002, 1e-9),
          repr(est["distance"]["distance_m"]))
    check("known_t0_absolute_2ms", close(est["absolute_from_t0"], 2.0, 1e-9), repr(est["absolute_from_t0"]))
    check("known_t0_relative_2ms", close(est["relative_to_segment"]["delay_ms"], 2.0, 1e-9))
    check("known_t0_usable", geo.distance_usable(est))

    # 2) 无公共 t0 -> 拒绝距离输出（事故根因回归）
    est_no_t0 = geo.estimate_direct_path(onset, geo.recording_only_timebase(), mic_role="room_listening")
    check("no_t0_status_INVALID", est_no_t0["distance"]["status"] == geo.STATUS_NO_TIMEBASE,
          est_no_t0["distance"]["status"])
    check("no_t0_distance_none", est_no_t0["distance"]["distance_m"] is None)
    check("no_t0_absolute_none", est_no_t0["absolute_from_t0"] is None)
    check("no_t0_not_usable", not geo.distance_usable(est_no_t0))
    check("no_t0_relative_still_available", close(est_no_t0["relative_to_segment"]["delay_ms"], 2.0, 1e-9))
    text = json.dumps(est_no_t0, ensure_ascii=False)
    check("no_legacy_ambiguous_field", geo.LEGACY_AMBIGUOUS_FIELD not in text, text)
    check("no_t0_json_distance_null", '"distance_m": null' in text, text)

    # 3) 事故复现：4 样本 0.083 ms，房间麦必须 SUSPECT（即使有公共 t0）
    ir_ma1 = synthetic_ir(4.0)
    onset_ma1 = geo.find_direct_onset(ir_ma1, FS, segment_start_sample=0.0)
    est_ma1 = geo.estimate_direct_path(onset_ma1, timebase, mic_role="room_listening")
    check("room_mic_0.083ms_SUSPECT", est_ma1["distance"]["status"] == geo.STATUS_SUSPECT,
          est_ma1["distance"]["status"])
    check("room_mic_not_usable", not geo.distance_usable(est_ma1))
    check("room_mic_distance_diagnostic",
          close(est_ma1["distance"]["distance_m"], 343.0 * 4.0 / FS, 1e-9))
    check("nearfield_allows_short", geo.distance_usable(
        geo.estimate_direct_path(onset_ma1, timebase, mic_role="nearfield")))
    check("loopback_reference_allows_short", geo.distance_usable(
        geo.estimate_direct_path(onset_ma1, timebase, mic_role="loopback_reference")))
    check("unknown_role_is_conservative", geo.estimate_direct_path(
        onset_ma1, timebase, mic_role="unknown")["distance"]["status"] == geo.STATUS_SUSPECT)

    # 4) 分数延迟 + 段起点偏移（t0 与段起点同域）
    onset_frac = geo.find_direct_onset(synthetic_ir(0.0, length=64), FS)  # dummy keep API exercised
    est_frac = geo.estimate_direct_path(
        {"peak_index": 96, "fractional_offset": 0.4, "segment_start_sample": 1000.0,
         "sample_rate_hz": FS},
        geo.known_timebase("known_playback_start", 1000.0, FS),
        mic_role="room_listening")
    check("fractional_valid", est_frac["distance"]["status"] == geo.STATUS_VALID)
    check("fractional_abs_ms", close(est_frac["absolute_from_t0"], 96.4 / FS * 1000.0, 1e-12))
    check("fractional_distance", close(est_frac["distance"]["distance_m"], 343.0 * 96.4 / FS, 1e-12))
    check("temperature_speed", close(geo.sound_speed_mps(20.0), 343.42, 1e-9))

    # 5) 时基/参数错误
    est_mismatch = geo.estimate_direct_path(
        {"peak_index": 96, "fractional_offset": None, "segment_start_sample": 0.0,
         "sample_rate_hz": FS},
        geo.known_timebase("loopback", 0.0, 44100.0))
    check("sample_rate_mismatch_INVALID", est_mismatch["distance"]["status"] == geo.STATUS_BAD_TIMEBASE,
          est_mismatch["distance"]["status"])
    est_negative = geo.estimate_direct_path(
        {"peak_index": 96, "fractional_offset": None, "segment_start_sample": 1000.0,
         "sample_rate_hz": FS},
        geo.known_timebase("loopback", 2000.0, FS))
    check("negative_delay_INVALID", est_negative["distance"]["status"] == geo.STATUS_NEGATIVE,
          est_negative["distance"]["status"])
    est_unit = geo.estimate_direct_path(
        {"peak_index": 96, "fractional_offset": None, "segment_start_sample": 0.0,
         "sample_rate_hz": FS},
        geo.known_timebase("loopback", 0.0, FS), speed_mps=0.343)
    check("speed_unit_error_INVALID", est_unit["distance"]["status"] == geo.STATUS_BAD_SPEED,
          est_unit["distance"]["status"])

    # 6) 参数校验必须 fail fast（不降级）
    for label, fn in (
        ("bad_timebase_source", lambda: geo.known_timebase("nope", 0.0, FS)),
        ("bad_t0", lambda: geo.known_timebase("loopback", -1.0, FS)),
        ("bad_timebase_rate", lambda: geo.known_timebase("loopback", 0.0, 0.0)),
        ("bad_mic_role", lambda: geo.estimate_direct_path(
            {"peak_index": 1, "fractional_offset": None, "segment_start_sample": 0.0,
             "sample_rate_hz": FS}, geo.recording_only_timebase(), mic_role="nope")),
        ("bad_fractional_offset", lambda: geo.relative_to_segment_ms(1, FS, 0.9)),
        ("bad_onset_rate", lambda: geo.find_direct_onset([1.0, 2.0], 0.0)),
    ):
        try:
            fn()
        except ValueError:
            check("raises_value_error_%s" % label, True)
        else:
            check("raises_value_error_%s" % label, False, "no ValueError")

    # 7) find_direct_onset 退化输入
    check("empty_ir_none", geo.find_direct_onset([], FS) is None)
    check("zero_ir_none", geo.find_direct_onset([0.0] * 128, FS) is None)
    peak = geo.find_direct_onset([0.0, 0.25, 1.0, 0.5, 0.0], FS, segment_start_sample=10.0)
    check("parabolic_offset_0.1", close(peak["fractional_offset"], 0.1, 1e-12), repr(peak))

    # 8) JSON 字段语义：只有 relative_to_segment/timebase/absolute_from_t0/distance，无旧歧义字段
    text_valid = json.dumps(est, ensure_ascii=False)
    for key in (geo.RELATIVE_TO_SEGMENT_FIELD, "timebase", geo.ABSOLUTE_FROM_T0_FIELD, geo.DISTANCE_FIELD):
        check("json_has_%s" % key, key in text_valid)
    check("json_no_legacy_field_anywhere", geo.LEGACY_AMBIGUOUS_FIELD not in text_valid)

    print("")
    print("SELFTEST SUMMARY: PASS=%d FAIL=%d" % (PASS, FAIL))
    if FAILURES:
        print("FAILURES: %s" % ", ".join(FAILURES))
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
