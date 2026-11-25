#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""[NONRT] make-multipoint-spec —— 多点 measured 设计 spec v2 离线生成/校验工具。

用途（目标 5：真实多点测量窗口 design>=6 + validation/holdout>=3 的离线预演与工具准备）：

1. synthetic    生成确定性合成 analysis/diag JSON 目录树（默认 6 design + 3 validation），
                全部内容带 SIMULATED 标注；seed/SHA 固定，可重现；
2. synthetic-ir 生成确定性合成 IR WAV（逐位置/逐通道），供 tspi-cal rt60 生成逐位置
                RT60 产物（SIMULATED；只用于工具链完整性预演）；
3. build        输入“按位置命名的 analysis JSON 目录”（如 D1/FL.json … V1/FR.json），
                逐文件校验 schema/fs/频响/SNR/clock/repeats 等质量门，输出
                tspi-cal-measured-design/v2 spec + sha256 manifest；
4. check        校验已有 spec v2 的结构、文件可达性与质量门（不重写 spec）；
5. selftest     工具自测（正向 + 负向 + 确定性），写 SELFTEST.json；
6. verify-e2e   读取离线全链（measured -> FitP -> compile -> plan-arena fake）证据目录，
                断言 plan design/validation 点数、host ok/blocks/acks、RT60 产物等，写 VERIFY.json。

诚实边界（工具不改变这些事实）：

- 本工具只读写本地文件，不打开设备、不出声、不碰 MOTU/dsp-host；
- synthetic/synthetic-ir 产物是合成数据，不是房间测量；manifest/REPORT 必须标 SIMULATED；
- 真实 6+3 测量窗口、T6/T68/T69 两轮复现、apply 后真实 holdout remeasure 仍 NOT_RUN；
- build 的质量门只做“测量产物是否满足流程门槛”的 fail-closed 检查；降低 --min-snr-db
  等阈值的绕过必须显式传参并会记入 manifest warning，不得据此声称验收通过。

用法（纯标准库，Python>=3.8）：

  python make_multipoint_spec.py synthetic --out PKG --design 6 --validation 3 --seed multipoint-prep-20261122
  python make_multipoint_spec.py build --input PKG --out PKG/spec.json --session-id multipoint-prep-6plus3 \
      --provenance simulated_deterministic_multipoint_6plus3
  python make_multipoint_spec.py check --spec PKG/spec.json --require-t6
  python make_multipoint_spec.py synthetic-ir --out PKG-ir --design 6 --validation 3
  python make_multipoint_spec.py selftest --out SELFTEST-DIR
  python make_multipoint_spec.py verify-e2e --run RUN --report REPORT --out VERIFY.json

退出码：0 通过；2 用法错误；3 输入/校验错误；4 输出写入错误。
"""

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import wave

TOOL_NAME = "make-multipoint-spec"
TOOL_VERSION = "1.0.0"

SPEC_SCHEMA_V2 = "tspi-cal-measured-design/v2"
SPEC_SCHEMA_V1 = "tspi-cal-measured-design/v1"
ANALYSIS_SCHEMA = "tspi-channel-measurement/v1"
DIAG_SCHEMA = "tspi-calibration-diag/v1"
MANIFEST_SCHEMA = "tspi-cal-multipoint-spec-manifest/v1"
SYNTH_SCHEMA = "tspi-cal-multipoint-synthetic-fixtures/v1"
POSITIONS_SCHEMA = "tspi-cal-multipoint-positions/v1"
VERIFY_SCHEMA = "tspi-cal-multipoint-prep-verify/v1"
VALIDATION_INDEPENDENT = "independent_validation_positions"

T6_MIN_DESIGN = 6
T6_MIN_VALIDATION = 3
MAX_DESIGN = 64
MAX_VALIDATION = 16
MAX_LABEL_CHARS = 64
MIN_FR_POINTS = 8
MIN_REPEATS_DEFAULT = 3
MIN_SNR_DB_DEFAULT = 25.0
MAX_CLOCK_PPM_SPREAD_DEFAULT = 5.0
CLOCK_PPM_SANITY = 100.0
RESIDUAL_SANITY_SAMPLES = 2.0
DEFAULT_RATE = 48000
CHANNEL_RE = re.compile(r"^[A-Z][A-Z0-9]*$")
LABEL_RE = re.compile(r"^[A-Za-z0-9_.-]+$")

EXIT_OK = 0
EXIT_USAGE = 2
EXIT_VALIDATION = 3
EXIT_OUTPUT = 4


# ---------------------------------------------------------------------------
# 稳定错误码（与证据/流程文档对应）
# ---------------------------------------------------------------------------
E_USAGE = "E_MP_USAGE"
E_INPUT = "E_MP_INPUT"
E_POSITIONS = "E_MP_POSITIONS"
E_ANALYSIS = "E_MP_ANALYSIS"
E_DIAG = "E_MP_DIAG"
E_QUALITY = "E_MP_QUALITY"
E_T6 = "E_MP_T6"
E_SPEC = "E_MP_SPEC"
E_OUTPUT = "E_MP_OUTPUT"
E_RUN = "E_MP_RUN"

W_SNR_MISSING = "W_MP_SNR_MISSING"
W_DIAG_MISSING = "W_MP_DIAG_MISSING"
W_OUTSIDE_SPEC_DIR = "W_MP_ANALYSIS_OUTSIDE_SPEC_DIR"
W_T6_BELOW = "W_MP_T6_BELOW"
W_SNR_THRESHOLD_LOWERED = "W_MP_SNR_THRESHOLD_LOWERED"
W_NON_MONOTONIC = "W_MP_FR_NOT_STRICTLY_MONOTONIC"
W_DIAG_SNR_BAND_DIFF = "W_MP_DIAG_SNR_BAND_DIFF"


class SpecError(Exception):
    """带稳定 code 的校验/输入错误。"""

    def __init__(self, code, message, details=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.details = list(details or [])


# ---------------------------------------------------------------------------
# 通用小工具
# ---------------------------------------------------------------------------
def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def natural_key(text):
    return [int(part) if part.isdigit() else part.lower() for part in re.split(r"(\d+)", str(text))]


def rel_posix(path, base_dir):
    """把 path 相对 base_dir 表示为 POSIX 风格（spec 内 analysis 路径口径）。"""
    rel = os.path.relpath(os.fspath(path), os.fspath(base_dir))
    return rel.replace(os.sep, "/")


def read_json(path):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        raise SpecError(E_INPUT, "文件不存在：%s" % path)
    except json.JSONDecodeError as exc:
        raise SpecError(E_INPUT, "JSON 解析失败 %s：%s" % (path, exc))
    except OSError as exc:
        raise SpecError(E_INPUT, "读文件失败 %s：%s" % (path, exc))


def write_text(path, text):
    try:
        parent = os.path.dirname(os.path.abspath(path))
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(path, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
    except OSError as exc:
        raise SpecError(E_OUTPUT, "写文件失败 %s：%s" % (path, exc))


def json_text(value, indent=2):
    return json.dumps(value, ensure_ascii=False, indent=indent) + "\n"


def stable_num(value, digits=9):
    """把浮点收敛到固定小数位，保证跨进程字节稳定（合成数据用）。"""
    return float("%.*f" % (digits, float(value)))


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(float(value))


def require(condition, code, message, details=None):
    if not condition:
        raise SpecError(code, message, details)


def add_warning(ctx, code, message):
    ctx["warnings"].append({"code": code, "message": message})


def new_ctx():
    return {"warnings": []}


# ---------------------------------------------------------------------------
# analysis / diag 校验
# ---------------------------------------------------------------------------
def inspect_analysis(path, fs, min_snr_db, ctx, where):
    """校验旧工具 analysis JSON（schema tspi-channel-measurement/v1）。

    返回 dict：sha256/bytes/channel/delay_samples/snr_band_db/points/频带。
    fail-closed：读不到/JSON 坏/schema 错/fs 不一致/fr 点不足/频点非法/频带过窄/
    SNR 不足（显式提供时）都是 E_MP_ANALYSIS 或 E_MP_QUALITY。
    SNR 口径：优先 analysis.snr_band_db（旧工具 1/48 oct 分析自带带限 SNR），
    否则 analysis.snr_db（200 Hz-8 kHz 口径）；diag.snr_band_db 带宽不同，只做提示不设门。
    """
    path = os.fspath(path)
    try:
        with open(path, "rb") as handle:
            raw = handle.read()
    except OSError as exc:
        raise SpecError(E_ANALYSIS, "%s：读 analysis 失败 %s：%s" % (where, path, exc))
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SpecError(E_ANALYSIS, "%s：analysis 非 UTF-8 %s：%s" % (where, path, exc))
    try:
        doc = json.loads(text)
    except json.JSONDecodeError as exc:
        raise SpecError(E_ANALYSIS, "%s：analysis JSON 解析失败 %s：%s" % (where, path, exc))
    require(isinstance(doc, dict), E_ANALYSIS, "%s：analysis 顶层必须是对象 %s" % (where, path))
    schema = doc.get("schema")
    require(
        schema == ANALYSIS_SCHEMA,
        E_ANALYSIS,
        "%s：analysis schema 必须是 %s（得到 %r）%s" % (where, ANALYSIS_SCHEMA, schema, path),
    )
    channel = doc.get("channel")
    require(
        isinstance(channel, int) and not isinstance(channel, bool) and 0 <= channel <= 65535,
        E_ANALYSIS,
        "%s：analysis 缺合法 channel（整数）%s" % (where, path),
    )
    meta = doc.get("meta")
    require(isinstance(meta, dict), E_ANALYSIS, "%s：analysis 缺 meta 对象 %s" % (where, path))
    rate = meta.get("sample_rate")
    require(
        isinstance(rate, int) and not isinstance(rate, bool),
        E_ANALYSIS,
        "%s：analysis 缺 meta.sample_rate（整数）%s" % (where, path),
    )
    require(
        int(rate) == int(fs),
        E_ANALYSIS,
        "%s：analysis fs=%s != spec sample_rate_hz=%s（测量 session 不一致）%s" % (where, rate, fs, path),
    )
    sweep_start = meta.get("sweep_start_hz", 20.0)
    sweep_end = meta.get("sweep_end_hz", 20000.0)
    require(
        is_number(sweep_start) and is_number(sweep_end) and float(sweep_end) > float(sweep_start),
        E_ANALYSIS,
        "%s：analysis sweep_start/end 非法 (%r..%r) %s" % (where, sweep_start, sweep_end, path),
    )
    low = max(20.0, float(sweep_start))
    high = min(20000.0, float(sweep_end))
    require(high > low * 2.0, E_ANALYSIS, "%s：公共频带过窄（%.3f–%.3f Hz）%s" % (where, low, high, path))
    delay = doc.get("delay_samples")
    require(is_number(delay), E_ANALYSIS, "%s：analysis 缺有限 delay_samples %s" % (where, path))
    fr = doc.get("fr")
    require(isinstance(fr, list), E_ANALYSIS, "%s：analysis 缺 fr 数组 %s" % (where, path))
    require(
        len(fr) >= MIN_FR_POINTS,
        E_ANALYSIS,
        "%s：analysis fr 只有 %d 点（< %d）%s" % (where, len(fr), MIN_FR_POINTS, path),
    )
    previous = -1.0
    for index, point in enumerate(fr):
        require(isinstance(point, dict), E_ANALYSIS, "%s：analysis fr[%d] 不是对象 %s" % (where, index, path))
        freq = point.get("freq_hz")
        db = point.get("db")
        phase = point.get("phase_deg")
        require(
            is_number(freq) and float(freq) > 0.0,
            E_ANALYSIS,
            "%s：analysis fr[%d].freq_hz 非法 %s" % (where, index, path),
        )
        require(
            is_number(db) and is_number(phase),
            E_ANALYSIS,
            "%s：analysis fr[%d].db/phase_deg 非有限数值 %s" % (where, index, path),
        )
        if index > 0 and float(freq) < previous:
            raise SpecError(
                E_ANALYSIS,
                "%s：analysis fr 频率非单调（fr[%d] %.6f < %.6f）%s" % (where, index, float(freq), previous, path),
            )
        if index > 0 and float(freq) == previous:
            add_warning(ctx, W_NON_MONOTONIC, "%s：analysis fr 有重复频点 %.6f %s" % (where, previous, path))
        previous = float(freq)
    snr_value = None
    snr_source = None
    if is_number(doc.get("snr_band_db")):
        snr_value = float(doc["snr_band_db"])
        snr_source = "snr_band_db"
    elif is_number(doc.get("snr_db")):
        snr_value = float(doc["snr_db"])
        snr_source = "snr_db(200Hz-8kHz 口径)"
    if snr_value is None:
        add_warning(ctx, W_SNR_MISSING, "%s：analysis 未提供 SNR（质量门不可判定）%s" % (where, path))
    else:
        require(
            snr_value >= min_snr_db,
            E_QUALITY,
            "%s：analysis %s=%.2f dB < 门槛 %.2f dB（T6 质量门；不得静默降门）%s"
            % (where, snr_source, snr_value, min_snr_db, path),
        )
    return {
        "path": path,
        "sha256": sha256_bytes(raw),
        "bytes": len(raw),
        "channel": int(channel),
        "delay_samples": float(delay),
        "snr_band_db": snr_value,
        "snr_source": snr_source,
        "points": len(fr),
        "sweep_start_hz": float(sweep_start),
        "sweep_end_hz": float(sweep_end),
    }


def inspect_diag(path, min_repeats, min_snr_db, ctx, where):
    """校验旧工具 diag JSON 的测量质量字段（重复次数/clock/onset/峰值嫌疑）。

    真实 diag（tspi-calibration-diag/v1）带 repeats/clock_ppm*/onset_invalid 等字段；
    这里把它们变成 fail-closed 的流程门。字段缺失按“不可判定”拒绝；只有整个 diag 缺失
    才能用 --allow-missing-diag 放宽（且记 warning）。
    """
    path = os.fspath(path)
    try:
        with open(path, "rb") as handle:
            raw = handle.read()
    except OSError as exc:
        raise SpecError(E_DIAG, "%s：读 diag 失败 %s：%s" % (where, path, exc))
    try:
        doc = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SpecError(E_DIAG, "%s：diag 解析失败 %s：%s" % (where, path, exc))
    require(isinstance(doc, dict), E_DIAG, "%s：diag 顶层必须是对象 %s" % (where, path))
    schema = doc.get("schema")
    if schema is not None and schema != DIAG_SCHEMA:
        raise SpecError(E_DIAG, "%s：diag schema=%r 不是 %s %s" % (where, schema, DIAG_SCHEMA, path))
    repeats = doc.get("repeats")
    require(
        isinstance(repeats, int) and not isinstance(repeats, bool),
        E_DIAG,
        "%s：diag 缺 repeats（整数；无法判定重复次数）%s" % (where, path),
    )
    require(
        int(repeats) >= int(min_repeats),
        E_DIAG,
        "%s：diag repeats=%d < 最少 %d（design/off 每点需 >=3 遍）%s" % (where, int(repeats), int(min_repeats), path),
    )
    ppm = doc.get("clock_ppm")
    require(is_number(ppm), E_DIAG, "%s：diag 缺有限 clock_ppm（无 clock 证据不可判定）%s" % (where, path))
    ppm = float(ppm)
    require(
        abs(ppm) <= CLOCK_PPM_SANITY,
        E_QUALITY,
        "%s：diag clock_ppm=%.3f 超出 sanity 上界 ±%.0f ppm %s" % (where, ppm, CLOCK_PPM_SANITY, path),
    )
    clock_ok = doc.get("clock_ppm_ok")
    require(
        isinstance(clock_ok, bool),
        E_DIAG,
        "%s：diag 缺 clock_ppm_ok（布尔；clock 无效遍不得进设计）%s" % (where, path),
    )
    require(clock_ok is True, E_QUALITY, "%s：diag clock_ppm_ok=false（该遍无效）%s" % (where, path))
    onset_invalid = doc.get("onset_invalid")
    require(isinstance(onset_invalid, bool), E_DIAG, "%s：diag 缺 onset_invalid（布尔）%s" % (where, path))
    require(onset_invalid is False, E_QUALITY, "%s：diag onset_invalid=true（onset 无效）%s" % (where, path))
    if "insufficient_valid_passes" in doc:
        require(
            doc.get("insufficient_valid_passes") is False,
            E_QUALITY,
            "%s：diag insufficient_valid_passes=true（有效遍数不足）%s" % (where, path),
        )
    suspects = doc.get("ir_peak_suspect")
    if isinstance(suspects, list):
        require(
            not any(bool(item) for item in suspects),
            E_DIAG,
            "%s：diag ir_peak_suspect 含 true（IR 峰值可疑）%s" % (where, path),
        )
    residuals = doc.get("residuals")
    if isinstance(residuals, list) and residuals:
        try:
            residual_max = max(abs(float(item)) for item in residuals)
        except (TypeError, ValueError):
            residual_max = None
        if residual_max is not None and residual_max > RESIDUAL_SANITY_SAMPLES:
            add_warning(
                ctx,
                "W_MP_ONSET_RESIDUAL",
                "%s：diag onset 残差最大 %.3f sample（> %.1f，需人工判读）%s"
                % (where, residual_max, RESIDUAL_SANITY_SAMPLES, path),
            )
    diag_snr = doc.get("snr_band_db")
    if is_number(diag_snr) and float(diag_snr) < min_snr_db:
        add_warning(
            ctx,
            W_DIAG_SNR_BAND_DIFF,
            "%s：diag snr_band_db=%.2f dB < %.2f dB；diag 自带带宽口径（如 20-20k）与 T6 200Hz-8kHz 门不同，"
            "仅提示，实际 SNR 门以 analysis 字段/复算为准 %s"
            % (where, float(diag_snr), min_snr_db, path),
        )
    return {
        "path": path,
        "sha256": sha256_bytes(raw),
        "bytes": len(raw),
        "repeats": int(repeats),
        "clock_ppm": ppm,
        "clock_ppm_ok": clock_ok,
        "onset_invalid": onset_invalid,
        "diag_snr_band_db": float(diag_snr) if is_number(diag_snr) else None,
    }


# ---------------------------------------------------------------------------
# 输入扫描（按位置命名的 analysis 目录）
# ---------------------------------------------------------------------------
TREE_ANALYSIS_RE = re.compile(r"^(?P<ch>[A-Z][A-Z0-9]*)\.(?:analysis\.)?json$")
TREE_DIAG_RE = re.compile(r"^(?P<ch>[A-Z][A-Z0-9]*)\.diag\.json$")
# 旧工具 tspi-calibrate measure-multi 的产物命名：FL-ch00.analysis.json / FL-ch00.diag.json
TREE_LEGACY_ANALYSIS_RE = re.compile(r"^(?P<ch>[A-Z][A-Z0-9]*)-ch[0-9]{2}\.analysis\.json$")
TREE_LEGACY_DIAG_RE = re.compile(r"^(?P<ch>[A-Z][A-Z0-9]*)-ch[0-9]{2}\.diag\.json$")
FLAT_ANALYSIS_RE = re.compile(r"^(?P<label>[A-Za-z0-9_.-]+)-(?P<ch>[A-Z][A-Z0-9]*)\.analysis\.json$")
FLAT_DIAG_RE = re.compile(r"^(?P<label>[A-Za-z0-9_.-]+)-(?P<ch>[A-Z][A-Z0-9]*)\.diag\.json$")


def scan_input(root, layout_mode, ctx):
    """扫描输入目录，返回 {label: {"analyses": {ch: path}, "diags": {ch: path}}}。

    支持两种布局：
    - tree（推荐，真实测量交接包）：ROOT/D1/FL.analysis.json、ROOT/D1/FL.json，
      或旧工具 measure-multi 原样产物 ROOT/D1/FL-ch00.analysis.json（+ FL-ch00.diag.json）；
    - flat（复用 multipoint_probe 旧夹具）：ROOT/D1-FL.analysis.json。
    """
    root = os.path.abspath(os.fspath(root))
    require(os.path.isdir(root), E_INPUT, "输入目录不存在：%s" % root)
    entries = [item for item in os.listdir(root) if not item.startswith(".")]
    dirs = [item for item in entries if os.path.isdir(os.path.join(root, item))]
    if layout_mode == "auto":
        layout_mode = "tree" if dirs else "flat"
    positions = {}

    def add(label, channel, kind, path):
        require(
            LABEL_RE.match(label) is not None and len(label) <= MAX_LABEL_CHARS,
            E_POSITIONS,
            "位置 label %r 非法（允许 [A-Za-z0-9_.-]，<=64 字符）" % label,
        )
        slot = positions.setdefault(label, {"analyses": {}, "diags": {}})
        bucket = slot["analyses"] if kind == "analysis" else slot["diags"]
        require(
            channel not in bucket,
            E_POSITIONS,
            "位置 %s 的通道 %s 出现重复文件（%s 与 %s）" % (label, channel, bucket.get(channel), path),
        )
        bucket[channel] = os.path.abspath(path)

    if layout_mode == "tree":
        require(dirs, E_INPUT, "tree 布局要求输入目录下存在按位置命名的子目录：%s" % root)
        for label in sorted(dirs, key=natural_key):
            directory = os.path.join(root, label)
            for name in sorted(os.listdir(directory)):
                full = os.path.join(directory, name)
                if not os.path.isfile(full):
                    continue
                match = TREE_DIAG_RE.match(name) or TREE_LEGACY_DIAG_RE.match(name)
                if match:
                    add(label, match.group("ch"), "diag", full)
                    continue
                match = TREE_ANALYSIS_RE.match(name) or TREE_LEGACY_ANALYSIS_RE.match(name)
                if match:
                    add(label, match.group("ch"), "analysis", full)
        # 根目录散落文件（spec.json / manifest 本身）忽略
    else:
        files = sorted(item for item in entries if os.path.isfile(os.path.join(root, item)))
        require(files, E_INPUT, "flat 布局要求输入目录下存在 <label>-<CH>.analysis.json：%s" % root)
        for name in files:
            match = FLAT_DIAG_RE.match(name)
            if match:
                add(match.group("label"), match.group("ch"), "diag", os.path.join(root, name))
                continue
            match = FLAT_ANALYSIS_RE.match(name)
            if match:
                add(match.group("label"), match.group("ch"), "analysis", os.path.join(root, name))
    require(positions, E_INPUT, "输入目录未发现任何 analysis JSON：%s（期望 <label>/<CH>.analysis.json）" % root)
    for label, slot in positions.items():
        require(slot["analyses"], E_INPUT, "位置 %s 没有任何 analysis JSON（仅有 diag）" % label)
    return positions


def load_positions_config(path):
    """读取窗口前冻结的测点表（label/role/weight/mic_orientation）。"""
    doc = read_json(path)
    require(isinstance(doc, dict), E_POSITIONS, "positions 文件顶层必须是对象：%s" % path)
    schema = doc.get("schema")
    if schema is not None and schema != POSITIONS_SCHEMA:
        raise SpecError(E_POSITIONS, "positions schema=%r 不是 %s" % (schema, POSITIONS_SCHEMA))
    entries = doc.get("positions")
    require(isinstance(entries, list) and entries, E_POSITIONS, "positions.positions 必须是非空数组")
    result = []
    seen = set()
    for index, entry in enumerate(entries):
        require(isinstance(entry, dict), E_POSITIONS, "positions[%d] 必须是对象" % index)
        label = entry.get("label")
        require(
            isinstance(label, str) and label and label == label.strip() and len(label) <= MAX_LABEL_CHARS,
            E_POSITIONS,
            "positions[%d].label 非法（非空、trim、<=64 字符）" % index,
        )
        require(label not in seen, E_POSITIONS, "positions label 重复：%s" % label)
        seen.add(label)
        role = entry.get("role")
        require(role in ("design", "validation"), E_POSITIONS, "positions %s role 必须是 design|validation" % label)
        weight = entry.get("weight", 1.0)
        require(is_number(weight) and float(weight) > 0.0, E_POSITIONS, "positions %s weight 必须有限且 >0" % label)
        orientation = entry.get("mic_orientation", "90deg")
        require(isinstance(orientation, str) and orientation, E_POSITIONS, "positions %s mic_orientation 非法" % label)
        result.append({"label": label, "role": role, "weight": float(weight), "mic_orientation": orientation})
    return result


def classify_positions(scanned_labels, design_labels, validation_labels, config):
    """确定 design/validation 归属、顺序与权重/朝向。"""
    if config is not None:
        cfg_labels = [item["label"] for item in config]
        missing = sorted(set(scanned_labels) - set(cfg_labels), key=natural_key)
        extra = sorted(set(cfg_labels) - set(scanned_labels), key=natural_key)
        require(
            not missing and not extra,
            E_POSITIONS,
            "positions 表与输入目录不一致",
            details=(
                (["输入有但 positions 表没有：%s" % ", ".join(missing)] if missing else [])
                + (["positions 表有但输入没有：%s" % ", ".join(extra)] if extra else [])
            ),
        )
        design = [item for item in config if item["role"] == "design"]
        validation = [item for item in config if item["role"] == "validation"]
        return design, validation

    if design_labels or validation_labels:
        design_set = set(design_labels or [])
        validation_set = set(validation_labels or [])
        overlap = design_set & validation_set
        require(not overlap, E_POSITIONS, "design/validation label 重叠：%s" % ", ".join(sorted(overlap)))
        unlisted = sorted(set(scanned_labels) - design_set - validation_set, key=natural_key)
        require(
            not unlisted,
            E_POSITIONS,
            "以下位置未显式归类（用 --design-labels/--validation-labels 或 positions 文件）：%s" % ", ".join(unlisted),
        )
        unknown = sorted((design_set | validation_set) - set(scanned_labels), key=natural_key)
        require(not unknown, E_POSITIONS, "显式 label 在输入目录不存在：%s" % ", ".join(unknown))
        design = [
            {"label": label, "role": "design", "weight": 1.0, "mic_orientation": "90deg"}
            for label in sorted(design_set, key=natural_key)
        ]
        validation = [
            {"label": label, "role": "validation", "weight": 1.0, "mic_orientation": "90deg"}
            for label in sorted(validation_set, key=natural_key)
        ]
        return design, validation

    design = []
    validation = []
    for label in sorted(scanned_labels, key=natural_key):
        prefix = label[0].upper()
        if prefix == "D":
            design.append({"label": label, "role": "design", "weight": 1.0, "mic_orientation": "90deg"})
        elif prefix == "V":
            validation.append({"label": label, "role": "validation", "weight": 1.0, "mic_orientation": "90deg"})
        else:
            raise SpecError(
                E_POSITIONS,
                "无法由位置名 %r 推断 design/validation（约定 D*/V*，或用 --design-labels/"
                "--validation-labels / --positions 文件显式归类）" % label,
            )
    return design, validation


# ---------------------------------------------------------------------------
# spec 基础段（非 position 字段）：默认 stereo-2.0 或从已有 spec v2 模板复制
# ---------------------------------------------------------------------------
DEFAULT_COMPILE = {
    "block_size": 128,
    "n_in": 2,
    "container_channels": 8,
    "low_band_d": 0,
    "l_g": 8,
    "l_p": 8,
}


def default_base(channels):
    require(
        channels == ["FL", "FR"],
        E_INPUT,
        "无 --template 时只支持通道 FL,FR（得到 %s）；4.1.2 等布局请给 --template <已有 spec v2>" % ",".join(channels),
    )
    return {
        "channel_bands": [
            {"channel": "FL", "band_hz": [80.0, 16000.0]},
            {"channel": "FR", "band_hz": [80.0, 16000.0]},
        ],
        "dense_to_dac": [0, 1],
        "roles": [
            {"role_index": 0, "role_id": "FL", "dac": 0, "outputs": [0]},
            {"role_index": 1, "role_id": "FR", "dac": 1, "outputs": [1]},
        ],
        "container_channels": 8,
        "unused_outputs": [2, 3, 4, 5, 6, 7],
        "compile": dict(DEFAULT_COMPILE),
        "modal_artifact": None,
    }


def base_from_template(path, channels):
    """从已有 v2 spec 复制接线/编译/频带等字段（positions 段由本工具重建）。"""
    doc = read_json(path)
    require(isinstance(doc, dict), E_SPEC, "template 顶层必须是对象：%s" % path)
    schema = doc.get("schema")
    require(schema == SPEC_SCHEMA_V2, E_SPEC, "template schema 必须是 %s（得到 %r）：%s" % (SPEC_SCHEMA_V2, schema, path))
    for key in ("roles", "dense_to_dac", "container_channels", "compile"):
        require(key in doc, E_SPEC, "template 缺字段 %s：%s" % (key, path))
    role_ids = []
    for role in doc["roles"]:
        require(isinstance(role, dict) and isinstance(role.get("role_id"), str), E_SPEC, "template.roles 项非法")
        role_ids.append(role["role_id"])
    require(
        sorted(role_ids) == sorted(channels),
        E_SPEC,
        "template roles=%s 与测量通道 %s 不一致（多点 spec 必须完全覆盖同一组通道角色）"
        % (",".join(role_ids), ",".join(channels)),
        details=["如确需子集测量，请另做 template 并记录该实验口径。"],
    )
    base = {
        "channel_bands": doc.get("channel_bands", []),
        "dense_to_dac": doc["dense_to_dac"],
        "roles": doc["roles"],
        "container_channels": doc["container_channels"],
        "unused_outputs": doc.get("unused_outputs", []),
        "compile": doc["compile"],
        "modal_artifact": doc.get("modal_artifact"),
    }
    if "fit_options" in doc:
        base["fit_options"] = doc["fit_options"]
    if "crossover" in doc:
        base["crossover"] = doc["crossover"]
    if isinstance(doc.get("layout"), str):
        base["_template_layout"] = doc["layout"]
    if isinstance(doc.get("target"), str):
        base["_template_target"] = doc["target"]
    if isinstance(doc.get("sample_rate_hz"), int):
        base["_template_rate"] = doc["sample_rate_hz"]
    return base


def canonical_channels(scanned, explicit_channels):
    """确定规范通道顺序：显式列表优先，否则按首个位置自然排序。"""
    first_label = sorted(scanned.keys(), key=natural_key)[0]
    inferred = sorted(scanned[first_label]["analyses"].keys(), key=natural_key)
    if explicit_channels:
        wanted = explicit_channels
        require(
            sorted(wanted) == sorted(inferred),
            E_POSITIONS,
            "--channels %s 与输入通道 %s 不一致" % (",".join(wanted), ",".join(inferred)),
        )
        return wanted
    for channel in inferred:
        require(
            CHANNEL_RE.match(channel) is not None,
            E_POSITIONS,
            "通道名 %r 非法（约定 ^[A-Z][A-Z0-9]*$，如 FL/FR/SUB1）" % channel,
        )
    return inferred


# ---------------------------------------------------------------------------
# build：扫描 -> 校验 -> 生成 spec v2 + manifest
# ---------------------------------------------------------------------------
def build_spec(args):
    ctx = new_ctx()
    if float(args.min_snr_db) < MIN_SNR_DB_DEFAULT:
        add_warning(
            ctx,
            W_SNR_THRESHOLD_LOWERED,
            "SNR 门槛被显式降到 %.2f dB（默认 %.2f；该 spec 不得据此声称 T6 质量门通过）"
            % (float(args.min_snr_db), MIN_SNR_DB_DEFAULT),
        )
    scanned = scan_input(args.input, args.layout_mode, ctx)
    config = load_positions_config(args.positions) if args.positions else None
    design_positions, validation_positions = classify_positions(
        list(scanned.keys()),
        [item for item in (args.design_labels or "").split(",") if item],
        [item for item in (args.validation_labels or "").split(",") if item],
        config,
    )
    design_count = len(design_positions)
    validation_count = len(validation_positions)
    require(design_count >= 1, E_POSITIONS, "design 位置为 0")
    require(
        validation_count >= 1,
        E_POSITIONS,
        "独立 validation 位置为 0：多点工具要求 design>=1 + validation>=1；单点 proxy 请用历史 v1 spec",
    )
    require(design_count <= MAX_DESIGN, E_POSITIONS, "design 位置 %d > 上限 %d" % (design_count, MAX_DESIGN))
    require(
        validation_count <= MAX_VALIDATION,
        E_POSITIONS,
        "validation 位置 %d > 上限 %d" % (validation_count, MAX_VALIDATION),
    )
    t6_met = design_count >= T6_MIN_DESIGN and validation_count >= T6_MIN_VALIDATION
    if args.allow_below_t6:
        if not t6_met:
            add_warning(
                ctx,
                W_T6_BELOW,
                "T6 最低点数未满足（design=%d/%d validation=%d/%d）；本 spec 只能做 pilot/实验，不得声称 T6 通过"
                % (design_count, T6_MIN_DESIGN, validation_count, T6_MIN_VALIDATION),
            )
    else:
        require(
            t6_met,
            E_T6,
            "T6 最低点数未满足：design=%d（需>=%d）validation=%d（需>=%d）；仅 pilot 可显式加 --allow-below-t6（且产物仍非 T6）"
            % (design_count, T6_MIN_DESIGN, validation_count, T6_MIN_VALIDATION),
        )

    channels = canonical_channels(scanned, [item for item in (args.channels or "").split(",") if item])
    positions = design_positions + validation_positions

    pinned = []
    clock_values = []
    for item in positions:
        label = item["label"]
        slot = scanned[label]
        require(
            sorted(slot["analyses"].keys()) == sorted(channels),
            E_POSITIONS,
            "位置 %s 的通道集合 %s != 规范通道集 %s（多点 spec 每个位置必须覆盖同一组通道）"
            % (label, ",".join(sorted(slot["analyses"])), ",".join(channels)),
        )
        require(
            item["mic_orientation"] == "90deg",
            E_POSITIONS,
            "位置 %s mic_orientation=%r：T63 听音位 plan 只接受 90deg（UMIK 麦轴垂直朝天花板）"
            % (label, item["mic_orientation"]),
        )
        channel_entries = []
        for channel in channels:
            analysis_path = slot["analyses"][channel]
            info = inspect_analysis(analysis_path, int(args.sample_rate), float(args.min_snr_db), ctx, "%s/%s" % (label, channel))
            diag_path = slot["diags"].get(channel)
            diag_info = None
            if diag_path is not None:
                diag_info = inspect_diag(diag_path, int(args.min_repeats), float(args.min_snr_db), ctx, "%s/%s" % (label, channel))
                clock_values.append((label, channel, diag_info["clock_ppm"]))
            else:
                if args.allow_missing_diag:
                    add_warning(
                        ctx,
                        W_DIAG_MISSING,
                        "%s/%s：缺 diag 文件（--allow-missing-diag 放行；clock/repeats 质量门不可判定）" % (label, channel),
                    )
                else:
                    raise SpecError(
                        E_DIAG,
                        "%s/%s：缺 diag JSON（<CH>.diag.json）：clock_ppm/repeats/onset 质量门不可判定；"
                        "如只是合成/旧夹具，显式加 --allow-missing-diag（会记 warning）" % (label, channel),
                    )
            channel_entries.append({"name": channel, "analysis": analysis_path, "info": info, "diag": diag_path, "diag_info": diag_info})
        pinned.append({"spec": item, "channels": channel_entries})

    if len(clock_values) >= 2:
        ppm_values = [item[2] for item in clock_values]
        spread = max(ppm_values) - min(ppm_values)
        require(
            spread <= float(args.max_clock_ppm_spread),
            E_QUALITY,
            "位置间 clock_ppm 跨度过大：%.3f ppm > 上限 %.3f ppm（同一测量 session 要求）" % (spread, float(args.max_clock_ppm_spread)),
            details=["逐文件 ppm：%s" % ", ".join("%s/%s=%+.3f" % (label, ch, ppm) for label, ch, ppm in clock_values)],
        )

    if args.template:
        base = base_from_template(args.template, channels)
    else:
        base = default_base(channels)
    layout = args.layout or base.pop("_template_layout", None) or "stereo-2.0"
    target = args.target or base.pop("_template_target", None) or "flat"
    sample_rate = int(args.sample_rate)
    base.pop("_template_layout", None)
    base.pop("_template_target", None)
    template_rate = base.pop("_template_rate", None)
    if template_rate is not None and int(template_rate) != sample_rate:
        raise SpecError(
            E_SPEC,
            "template sample_rate_hz=%s 与 --sample-rate %s 不一致（同一 session 必须同 fs）" % (template_rate, sample_rate),
        )

    spec_dir = os.path.dirname(os.path.abspath(args.out))
    analysis_base = spec_dir

    def render_position(entry):
        spec_item = entry["spec"]
        channels_out = []
        for channel in entry["channels"]:
            rel = rel_posix(channel["analysis"], analysis_base)
            if rel.startswith("../"):
                add_warning(
                    ctx,
                    W_OUTSIDE_SPEC_DIR,
                    "%s/%s analysis 落在 spec 目录之外（%s）：交接/上传时请保持相对目录结构，或把 spec 放到测量根目录"
                    % (spec_item["label"], channel["name"], rel),
                )
            item = {"name": channel["name"], "analysis": rel}
            if channel["diag"] is not None:
                item["diag"] = rel_posix(channel["diag"], analysis_base)
            channels_out.append(item)
        return {
            "label": spec_item["label"],
            "mic_orientation": spec_item["mic_orientation"],
            "weight": float(spec_item["weight"]),
            "channels": channels_out,
        }

    spec_doc = {
        "schema": SPEC_SCHEMA_V2,
        "session_id": args.session_id,
        "layout": layout,
        "sample_rate_hz": sample_rate,
        "target": target,
        "validation": VALIDATION_INDEPENDENT,
        "provenance": args.provenance,
        "design_positions": [render_position(entry) for entry in pinned[:design_count]],
        "validation_positions": [render_position(entry) for entry in pinned[design_count:]],
        "channel_bands": base.get("channel_bands", []),
        "dense_to_dac": base["dense_to_dac"],
        "roles": base["roles"],
        "container_channels": base["container_channels"],
        "unused_outputs": base.get("unused_outputs", []),
        "compile": base["compile"],
        "modal_artifact": base.get("modal_artifact"),
    }
    for optional in ("fit_options", "crossover"):
        if optional in base:
            spec_doc[optional] = base[optional]
    if args.stamp:
        spec_doc["generated_utc"] = args.stamp

    spec_text = json_text(spec_doc)
    spec_bytes = spec_text.encode("utf-8")
    spec_sha = sha256_bytes(spec_bytes)
    write_text(args.out, spec_text)

    simulated = bool(args.simulated) or any(
        token in args.provenance.lower() for token in ("simulat", "synthetic", "fake", "offline")
    )
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION},
        "spec": os.path.basename(args.out),
        "spec_sha256": spec_sha,
        "session_id": args.session_id,
        "layout": layout,
        "target": target,
        "sample_rate_hz": sample_rate,
        "counts": {"design": design_count, "validation": validation_count, "total": design_count + validation_count},
        "t6_minimums": {
            "design_required": T6_MIN_DESIGN,
            "validation_required": T6_MIN_VALIDATION,
            "design_count": design_count,
            "validation_count": validation_count,
            "points_met": t6_met,
            "status": "POINTS_MET_NOT_ACCEPTED" if t6_met else "POINTS_BELOW_T6_MINIMUM",
            "note": "点数满足也不等于 T6 验收：真实测量窗口 + holdout remeasure + 两轮复现仍 NOT_RUN",
        },
        "positions": [
            {
                "label": entry["spec"]["label"],
                "role": entry["spec"]["role"],
                "weight": float(entry["spec"]["weight"]),
                "mic_orientation": entry["spec"]["mic_orientation"],
                "channels": [
                    {
                        "name": channel["name"],
                        "analysis": rel_posix(channel["analysis"], analysis_base),
                        "analysis_sha256": channel["info"]["sha256"],
                        "analysis_bytes": channel["info"]["bytes"],
                        "points": channel["info"]["points"],
                        "delay_samples": channel["info"]["delay_samples"],
                        "snr_band_db": channel["info"]["snr_band_db"],
                        "diag": rel_posix(channel["diag"], analysis_base) if channel["diag"] else None,
                        "diag_sha256": channel["diag_info"]["sha256"] if channel["diag_info"] else None,
                        "repeats": channel["diag_info"]["repeats"] if channel["diag_info"] else None,
                        "clock_ppm": channel["diag_info"]["clock_ppm"] if channel["diag_info"] else None,
                    }
                    for channel in entry["channels"]
                ],
            }
            for entry in pinned
        ],
        "checks": {"errors": [], "warnings": ctx["warnings"]},
        "simulated": simulated,
        "provenance": args.provenance,
        "measurement_input": "synthetic_offline" if simulated else "operator-declared real measurement input",
        "note": "多点 spec v2 生成证据；真实多点测量窗口/T6/T68 仍 NOT_RUN；holdout 不参与拟合",
    }
    if args.stamp:
        manifest["generated_utc"] = args.stamp
    write_text(args.manifest, json_text(manifest))
    return {
        "command": "build",
        "spec": os.path.abspath(args.out),
        "manifest": os.path.abspath(args.manifest),
        "spec_sha256": spec_sha,
        "design": design_count,
        "validation": validation_count,
        "t6_points_met": t6_met,
        "t6_status": manifest["t6_minimums"]["status"],
        "simulated": simulated,
        "warnings": len(ctx["warnings"]),
    }


# ---------------------------------------------------------------------------
# synthetic：确定性合成 analysis/diag 夹具（SIMULATED）
# ---------------------------------------------------------------------------
def _hash_floats(seed, *parts):
    digest = hashlib.sha256(("\x1f".join([seed] + [str(part) for part in parts])).encode("utf-8")).digest()
    return [
        int.from_bytes(digest[index * 8 : (index + 1) * 8], "big") / float(1 << 64)
        for index in range(4)
    ]


def _bump(freq, center_hz, gain_db, oct_width):
    distance = math.log(freq / center_hz, 2.0) / oct_width
    return gain_db * math.exp(-0.5 * distance * distance)


def _freq_grid(start_hz=20.0, end_hz=20000.0, points_per_oct=24):
    grid = []
    index = 0
    while True:
        freq = start_hz * (2.0 ** (index / float(points_per_oct)))
        if freq > end_hz:
            break
        grid.append(stable_num(freq, 7))
        index += 1
    return grid


def design_shape_db(freq, label_index, values):
    """design 位置合成形状：共享房间模态 + 座位差异 + 少量座位私有模态。"""
    gain100 = 6.0 + (values[0] - 0.5) * 2.2
    gain160 = 4.0 + (values[1] - 0.5) * 1.8
    value = _bump(freq, 100.0, gain100, 0.08) + _bump(freq, 160.0, gain160, 0.10) + _bump(freq, 1200.0, -1.5, 0.5)
    if label_index % 3 == 1:
        value += _bump(freq, 300.0 + 25.0 * label_index, 4.0 + (values[2] - 0.5) * 1.0, 0.05)
    elif label_index % 3 == 2:
        value += _bump(freq, 520.0 + 20.0 * label_index, 3.0 + (values[2] - 0.5) * 1.0, 0.06)
    value += (values[3] - 0.5) * 0.6 * math.log(max(freq, 1.0) / 1000.0, 2.0) / 8.0
    return value


def validation_shape_db(freq, label_index, values):
    """validation 位置合成形状：与 design 明显不同（1 kHz 下陷 + 不同模态），保证 holdout 有意义。"""
    gain100 = 3.0 + (values[0] - 0.5) * 1.6
    gain160 = 1.8 + (values[1] - 0.5) * 1.2
    return (
        -0.5
        + _bump(freq, 100.0, gain100, 0.10)
        + _bump(freq, 160.0, gain160, 0.12)
        + _bump(freq, 1000.0, -4.0 + (values[2] - 0.5) * 1.5, 0.30)
        + _bump(freq, 2500.0, 1.5 + (values[3] - 0.5) * 1.0, 0.40)
    )


def _analysis_document(channel_index, delay_samples, freqs, shape, sample_rate):
    fr = [{"freq_hz": freq, "db": stable_num(shape(freq), 7), "phase_deg": 0.0} for freq in freqs]
    return {
        "schema": ANALYSIS_SCHEMA,
        "channel": int(channel_index),
        "delay_samples": stable_num(delay_samples, 6),
        "snr_band_db": 31.0,
        "polarity_confidence": "high",
        "meta": {"sample_rate": int(sample_rate), "sweep_start_hz": 20.0, "sweep_end_hz": 20000.0},
        "fr": fr,
    }


def _diag_document(channel_index, name, delay_samples, seed, label, channel, sample_rate):
    values = _hash_floats(seed, "diag", label, channel)
    ppm = 2.0 + (values[0] - 0.5) * 0.8
    spacing = float(sample_rate) * 5.2
    starts = [
        stable_num(0.35 * float(sample_rate) + delay_samples + index * (spacing + (values[1] - 0.5) * 0.12), 6)
        for index in range(3)
    ]
    return {
        "schema": DIAG_SCHEMA,
        "channel": int(channel_index),
        "name": name,
        "slot": int(channel_index),
        "repeats": 3,
        "repeat_starts": starts,
        "spacing_samples": stable_num(spacing, 6),
        "residuals": [0.8, -0.8, 0.8],
        "accepted_diffs": [stable_num(spacing, 6), stable_num(spacing + 0.12, 6)],
        "rejected_diffs": [],
        "clock_ppm": stable_num(ppm, 6),
        "clock_ppm_uncertainty_ppm": 0.3,
        "clock_ppm_ok": True,
        "clock_ppm_significant": True,
        "onset_invalid": False,
        "insufficient_valid_passes": False,
        "ir_peak_suspect": [False, False, False],
        "nominal_start": 0.0,
        "actual_start": stable_num(delay_samples, 6),
        "snr_band_db": 31.0,
        "snr_band_hz": [20.0, 20000.0],
        "capture_file": "SIMULATED/offline/capture-%s-%s.raw" % (label, channel),
        "capture2_file": "SIMULATED/offline/capture2-%s-%s.raw" % (label, channel),
        "note": "SIMULATED deterministic fixture for offline rehearsal; not a real room measurement",
    }


def synth_package(out_dir, design_count, validation_count, channels, seed, rate, force):
    out_dir = os.path.abspath(out_dir)
    if os.path.isdir(out_dir) and os.listdir(out_dir):
        require(force, E_OUTPUT, "输出目录已存在且非空：%s（确认覆盖请加 --force）" % out_dir)
        shutil.rmtree(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    freqs = _freq_grid()
    specs = [("design", "D%d" % (index + 1), index) for index in range(design_count)]
    specs += [("validation", "V%d" % (index + 1), index) for index in range(validation_count)]
    files = []
    for role, label, index in specs:
        directory = os.path.join(out_dir, label)
        os.makedirs(directory, exist_ok=True)
        for channel_index, channel in enumerate(channels):
            values = _hash_floats(seed, role, label, channel)
            if role == "design":
                shape = lambda freq, values=values, index=index: design_shape_db(freq, index, values)
            else:
                shape = lambda freq, values=values, index=index: validation_shape_db(freq, index, values)
            delay = 96.0 + 2.0 * (index % 3) + (0.0 if channel == channels[0] else 24.0)
            analysis_doc = _analysis_document(channel_index, delay, freqs, shape, rate)
            diag_doc = _diag_document(channel_index, channel, delay, seed, label, channel, rate)
            analysis_path = os.path.join(directory, "%s.json" % channel)
            diag_path = os.path.join(directory, "%s.diag.json" % channel)
            analysis_text = json.dumps(analysis_doc, ensure_ascii=False, separators=(",", ":")) + "\n"
            diag_text = json.dumps(diag_doc, ensure_ascii=False, separators=(",", ":")) + "\n"
            write_text(analysis_path, analysis_text)
            write_text(diag_path, diag_text)
            files.append(
                {
                    "path": "%s/%s.json" % (label, channel),
                    "sha256": sha256_bytes(analysis_text.encode("utf-8")),
                    "bytes": len(analysis_text.encode("utf-8")),
                    "role": role,
                }
            )
            files.append(
                {
                    "path": "%s/%s.diag.json" % (label, channel),
                    "sha256": sha256_bytes(diag_text.encode("utf-8")),
                    "bytes": len(diag_text.encode("utf-8")),
                    "role": role,
                }
            )
    manifest = {
        "schema": SYNTH_SCHEMA,
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION},
        "seed": seed,
        "simulated": True,
        "note": "SIMULATED deterministic fixtures (not a real room measurement; no microphone/device involved)",
        "sample_rate_hz": int(rate),
        "channels": channels,
        "design": design_count,
        "validation": validation_count,
        "t6_minimums": {
            "design_required": T6_MIN_DESIGN,
            "validation_required": T6_MIN_VALIDATION,
            "points_met": design_count >= T6_MIN_DESIGN and validation_count >= T6_MIN_VALIDATION,
        },
        "files": files,
    }
    write_text(os.path.join(out_dir, "synthetic-manifest.json"), json_text(manifest))
    return {
        "command": "synthetic",
        "out": out_dir,
        "design": design_count,
        "validation": validation_count,
        "files": len(files),
        "seed": seed,
        "simulated": True,
        "manifest": os.path.join(out_dir, "synthetic-manifest.json"),
    }


# ---------------------------------------------------------------------------
# synthetic-ir：确定性合成 IR WAV（逐位置/逐通道，SIMULATED）
# ---------------------------------------------------------------------------
def _ir_samples(seed, label, channel, rate, length_seconds):
    values = _hash_floats(seed, "ir", label, channel)
    rt60 = 0.30 + values[0] * 0.30
    length = int(round(rate * length_seconds))
    onset = int(round(rate * 0.010))
    samples = [0.0] * length
    state = int.from_bytes(hashlib.sha256(("ir-noise|%s|%s|%s" % (seed, label, channel)).encode()).digest()[:8], "big")
    state = (state | 1) & 0xFFFFFFFFFFFFFFFF
    decay = math.exp(-math.log(1000.0) / (rt60 * rate))
    envelope = 1.0
    samples[onset] = 1.0
    for index in range(onset, length):
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        uniform = ((state >> 11) & ((1 << 53) - 1)) / float(1 << 53)
        samples[index] = (uniform * 2.0 - 1.0) * envelope * 0.18
        envelope *= decay
    for index in range(length):
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        uniform = ((state >> 11) & ((1 << 53) - 1)) / float(1 << 53)
        samples[index] += (uniform * 2.0 - 1.0) * 0.00002
    return samples, rt60


def synth_ir(out_dir, design_count, validation_count, channels, seed, rate, length_seconds, force):
    out_dir = os.path.abspath(out_dir)
    if os.path.isdir(out_dir) and os.listdir(out_dir):
        require(force, E_OUTPUT, "输出目录已存在且非空：%s（确认覆盖请加 --force）" % out_dir)
        shutil.rmtree(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    specs = [("design", "D%d" % (index + 1)) for index in range(design_count)]
    specs += [("validation", "V%d" % (index + 1)) for index in range(validation_count)]
    files = []
    for role, label in specs:
        directory = os.path.join(out_dir, label)
        os.makedirs(directory, exist_ok=True)
        for channel in channels:
            samples, rt60 = _ir_samples(seed, label, channel, rate, length_seconds)
            path = os.path.join(directory, "%s.wav" % channel)
            try:
                with wave.open(path, "wb") as handle:
                    handle.setnchannels(1)
                    handle.setsampwidth(2)
                    handle.setframerate(int(rate))
                    frames = bytearray()
                    for value in samples:
                        clamped = max(-1.0, min(1.0, value))
                        frames += int(round(clamped * 32767.0)).to_bytes(2, "little", signed=True)
                    handle.writeframes(bytes(frames))
            except OSError as exc:
                raise SpecError(E_OUTPUT, "写 WAV 失败 %s：%s" % (path, exc))
            files.append(
                {
                    "label": "%s_%s" % (label, channel),
                    "role": role,
                    "position": label,
                    "channel": channel,
                    "path": "%s/%s.wav" % (label, channel),
                    "sha256": sha256_file(path),
                    "bytes": os.path.getsize(path),
                    "target_rt60_s": stable_num(rt60, 6),
                }
            )
    manifest = {
        "schema": "tspi-cal-multipoint-synthetic-ir/v1",
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION},
        "seed": seed,
        "simulated": True,
        "note": "SIMULATED deterministic IR WAVs (not a real room measurement); for RT60 artifact tooling rehearsal only",
        "sample_rate_hz": int(rate),
        "channels": channels,
        "design": design_count,
        "validation": validation_count,
        "files": files,
        "label_order": [item["label"] for item in files],
    }
    write_text(os.path.join(out_dir, "synthetic-ir-manifest.json"), json_text(manifest))
    return {
        "command": "synthetic-ir",
        "out": out_dir,
        "design": design_count,
        "validation": validation_count,
        "files": len(files),
        "simulated": True,
        "label_order": manifest["label_order"],
    }


# ---------------------------------------------------------------------------
# check：校验已有 spec v2
# ---------------------------------------------------------------------------
def check_spec(spec_path, require_t6, min_snr_db, min_repeats, allow_missing_diag):
    ctx = new_ctx()
    spec_path = os.path.abspath(os.fspath(spec_path))
    doc = read_json(spec_path)
    require(isinstance(doc, dict), E_SPEC, "spec 顶层必须是对象")
    require(doc.get("schema") == SPEC_SCHEMA_V2, E_SPEC, "spec schema 必须为 %s（得到 %r）" % (SPEC_SCHEMA_V2, doc.get("schema")))
    for key in ("session_id", "layout", "target", "provenance", "validation", "sample_rate_hz"):
        require(key in doc, E_SPEC, "spec 缺字段 %s" % key)
    fs = doc.get("sample_rate_hz")
    require(isinstance(fs, int) and not isinstance(fs, bool) and fs > 0, E_SPEC, "spec sample_rate_hz 非法")
    design = doc.get("design_positions")
    validation = doc.get("validation_positions")
    require(isinstance(design, list) and design, E_SPEC, "spec design_positions 必须是非空数组")
    require(isinstance(validation, list) and validation, E_SPEC, "spec validation_positions 必须是非空数组（多点禁止 proxy）")
    require(
        doc.get("validation") == VALIDATION_INDEPENDENT,
        E_SPEC,
        "有 validation_positions[] 时 validation 必须为 %s（得到 %r）" % (VALIDATION_INDEPENDENT, doc.get("validation")),
    )
    design_count, validation_count = len(design), len(validation)
    require(design_count <= MAX_DESIGN, E_SPEC, "design 位置 %d > 上限 %d" % (design_count, MAX_DESIGN))
    require(validation_count <= MAX_VALIDATION, E_SPEC, "validation 位置 %d > 上限 %d" % (validation_count, MAX_VALIDATION))
    t6_met = design_count >= T6_MIN_DESIGN and validation_count >= T6_MIN_VALIDATION
    if require_t6:
        require(t6_met, E_T6, "T6 最低点数未满足：design=%d validation=%d（需 6+3）" % (design_count, validation_count))
    labels = []
    channel_set = None
    base_dir = os.path.dirname(spec_path)
    positions_out = []
    for role, items in (("design", design), ("validation", validation)):
        for index, item in enumerate(items):
            require(isinstance(item, dict), E_SPEC, "%s_positions[%d] 必须是对象" % (role, index))
            label = item.get("label")
            require(
                isinstance(label, str) and label and label == label.strip() and len(label) <= MAX_LABEL_CHARS,
                E_SPEC,
                "%s_positions[%d].label 非法" % (role, index),
            )
            require(label not in labels, E_SPEC, "测点 label 重复：%s" % label)
            labels.append(label)
            require(item.get("mic_orientation") == "90deg", E_SPEC, "位置 %s mic_orientation 必须为 90deg" % label)
            weight = item.get("weight", 1.0)
            require(is_number(weight) and float(weight) > 0.0, E_SPEC, "位置 %s weight 必须有限且 >0" % label)
            channels = item.get("channels")
            require(isinstance(channels, list) and channels, E_SPEC, "位置 %s channels 必须是非空数组" % label)
            names = []
            for channel in channels:
                require(isinstance(channel, dict), E_SPEC, "位置 %s channels 项必须是对象" % label)
                name = channel.get("name")
                require(isinstance(name, str) and CHANNEL_RE.match(name) is not None, E_SPEC, "位置 %s 通道名 %r 非法" % (label, name))
                require(name not in names, E_SPEC, "位置 %s 通道 %s 重复" % (label, name))
                names.append(name)
            if channel_set is None:
                channel_set = sorted(names)
            else:
                require(
                    sorted(names) == channel_set,
                    E_SPEC,
                    "位置 %s 通道集合 %s != %s（每位置必须同一组通道）" % (label, ",".join(sorted(names)), ",".join(channel_set)),
                )
            entries = []
            for channel in channels:
                name = channel["name"]
                analysis_field = channel.get("analysis")
                require(isinstance(analysis_field, str) and analysis_field, E_SPEC, "位置 %s/%s 缺 analysis 路径" % (label, name))
                analysis_path = analysis_field if os.path.isabs(analysis_field) else os.path.join(base_dir, analysis_field)
                info = inspect_analysis(analysis_path, int(fs), float(min_snr_db), ctx, "%s/%s" % (label, name))
                diag_field = channel.get("diag")
                diag_info = None
                if isinstance(diag_field, str) and diag_field:
                    diag_path = diag_field if os.path.isabs(diag_field) else os.path.join(base_dir, diag_field)
                    diag_info = inspect_diag(diag_path, int(min_repeats), float(min_snr_db), ctx, "%s/%s" % (label, name))
                elif not allow_missing_diag:
                    raise SpecError(E_DIAG, "位置 %s/%s 缺 diag（spec 无 diag 字段且未 --allow-missing-diag）" % (label, name))
                entries.append({"name": name, "analysis": info, "diag": diag_info})
            positions_out.append({"label": label, "role": role, "weight": float(weight), "channels": entries})
    for key in ("dense_to_dac", "roles", "container_channels", "compile"):
        require(key in doc, E_SPEC, "spec 缺接线/编译字段 %s" % key)
    return {
        "command": "check",
        "spec": spec_path,
        "design": design_count,
        "validation": validation_count,
        "t6_points_met": t6_met,
        "positions": positions_out,
        "checks": {"warnings": ctx["warnings"]},
        "ok": True,
    }


# ---------------------------------------------------------------------------
# selftest：正向/负向/确定性自测
# ---------------------------------------------------------------------------
def _run_tool(argv, expect_exit, expect_code=None):
    command = [sys.executable, os.path.abspath(__file__)] + list(argv)
    env = dict(os.environ)
    env["PYTHONUTF8"] = "1"
    proc = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env)
    ok = proc.returncode == expect_exit
    if expect_code is not None:
        ok = ok and (expect_code in (proc.stderr or ""))
    return {
        "argv": argv,
        "exit": proc.returncode,
        "expected_exit": expect_exit,
        "expected_code": expect_code,
        "pass": ok,
        "stdout_tail": (proc.stdout or "")[-2000:],
        "stderr_tail": (proc.stderr or "")[-2000:],
    }


def cmd_selftest(args):
    out_dir = os.path.abspath(args.out)
    if os.path.isdir(out_dir) and os.listdir(out_dir):
        require(args.force, E_OUTPUT, "selftest 输出目录已存在且非空：%s（加 --force 覆盖）" % out_dir)
        shutil.rmtree(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    seed = "multipoint-selftest-20261122"
    cases = []

    fixture_dir = os.path.join(out_dir, "synthetic")
    cases.append(dict(name="synthetic-6plus3", **_run_tool(["synthetic", "--out", fixture_dir, "--design", "6", "--validation", "3", "--seed", seed], 0)))
    require(cases[-1]["pass"], E_RUN, "selftest: synthetic 生成失败", [cases[-1]["stderr_tail"]])

    spec_a = os.path.join(fixture_dir, "spec.json")
    manifest_a = os.path.join(fixture_dir, "spec-manifest.json")
    cases.append(
        dict(
            name="build-6plus3",
            **_run_tool(
                ["build", "--input", fixture_dir, "--out", spec_a, "--manifest", manifest_a, "--session-id", "selftest-6plus3", "--provenance", "simulated_selftest"],
                0,
            ),
        )
    )
    require(cases[-1]["pass"], E_RUN, "selftest: build 6+3 失败", [cases[-1]["stderr_tail"]])
    spec_bytes_a = open(spec_a, "rb").read()
    manifest_a_doc = read_json(manifest_a)
    cases.append(
        {
            "name": "manifest-counts",
            "pass": manifest_a_doc["counts"] == {"design": 6, "validation": 3, "total": 9}
            and manifest_a_doc["t6_minimums"]["points_met"] is True
            and manifest_a_doc["simulated"] is True,
            "detail": "counts/t6/simulated",
        }
    )

    # 确定性对照：第二个 spec 放在同一目录（相对 analysis 路径一致），文件名字不同不影响 spec 内容
    spec_b = os.path.join(fixture_dir, "spec-determinism.json")
    manifest_b = os.path.join(fixture_dir, "spec-determinism.manifest.json")
    cases.append(
        dict(
            name="build-deterministic",
            **_run_tool(
                ["build", "--input", fixture_dir, "--out", spec_b, "--manifest", manifest_b, "--session-id", "selftest-6plus3", "--provenance", "simulated_selftest"],
                0,
            ),
        )
    )
    cases.append({"name": "spec-bytes-identical", "pass": cases[-1]["pass"] and open(spec_b, "rb").read() == spec_bytes_a, "detail": "两次 build 的 spec 字节一致"})

    check_json = os.path.join(out_dir, "check", "check.json")
    cases.append(dict(name="check-spec", **_run_tool(["check", "--spec", spec_a, "--require-t6", "--json", "--out", check_json], 0)))

    below_dir = os.path.join(out_dir, "negative", "below-t6")
    cases.append(dict(name="synthetic-5plus3", **_run_tool(["synthetic", "--out", below_dir, "--design", "5", "--validation", "3", "--seed", seed], 0)))
    cases.append(dict(name="build-below-t6-rejected", **_run_tool(["build", "--input", below_dir, "--out", os.path.join(below_dir, "spec.json")], 3, E_T6)))
    cases.append(dict(name="build-below-t6-allowed", **_run_tool(["build", "--input", below_dir, "--out", os.path.join(below_dir, "spec.json"), "--allow-below-t6"], 0)))

    no_diag_dir = os.path.join(out_dir, "negative", "no-diag")
    shutil.copytree(fixture_dir, no_diag_dir, ignore=shutil.ignore_patterns("*.diag.json", "spec*.json", "synthetic-manifest.json"))
    cases.append(dict(name="build-missing-diag-rejected", **_run_tool(["build", "--input", no_diag_dir, "--out", os.path.join(no_diag_dir, "spec.json")], 3, E_DIAG)))
    cases.append(dict(name="build-missing-diag-allowed", **_run_tool(["build", "--input", no_diag_dir, "--out", os.path.join(no_diag_dir, "spec.json"), "--allow-missing-diag"], 0)))

    bad_fs_dir = os.path.join(out_dir, "negative", "bad-fs")
    shutil.copytree(fixture_dir, bad_fs_dir, ignore=shutil.ignore_patterns("spec*.json", "synthetic-manifest.json"))
    bad_file = os.path.join(bad_fs_dir, "D1", "FL.json")
    bad_doc = read_json(bad_file)
    bad_doc["meta"]["sample_rate"] = 44100
    write_text(bad_file, json.dumps(bad_doc, ensure_ascii=False))
    cases.append(dict(name="build-fs-mismatch-rejected", **_run_tool(["build", "--input", bad_fs_dir, "--out", os.path.join(bad_fs_dir, "spec.json")], 3, E_ANALYSIS)))

    bad_clock_dir = os.path.join(out_dir, "negative", "bad-clock")
    shutil.copytree(fixture_dir, bad_clock_dir, ignore=shutil.ignore_patterns("spec*.json", "synthetic-manifest.json"))
    bad_diag_file = os.path.join(bad_clock_dir, "D2", "FR.diag.json")
    bad_diag = read_json(bad_diag_file)
    bad_diag["clock_ppm"] = 40.0
    write_text(bad_diag_file, json.dumps(bad_diag, ensure_ascii=False))
    cases.append(dict(name="build-clock-spread-rejected", **_run_tool(["build", "--input", bad_clock_dir, "--out", os.path.join(bad_clock_dir, "spec.json")], 3, E_QUALITY)))

    bad_repeats_dir = os.path.join(out_dir, "negative", "bad-repeats")
    shutil.copytree(fixture_dir, bad_repeats_dir, ignore=shutil.ignore_patterns("spec*.json", "synthetic-manifest.json"))
    bad_repeats_file = os.path.join(bad_repeats_dir, "V1", "FL.diag.json")
    bad_repeats = read_json(bad_repeats_file)
    bad_repeats["repeats"] = 1
    write_text(bad_repeats_file, json.dumps(bad_repeats, ensure_ascii=False))
    cases.append(dict(name="build-repeats-rejected", **_run_tool(["build", "--input", bad_repeats_dir, "--out", os.path.join(bad_repeats_dir, "spec.json")], 3, E_DIAG)))

    bad_orientation_dir = os.path.join(out_dir, "negative", "bad-orientation")
    shutil.copytree(fixture_dir, bad_orientation_dir, ignore=shutil.ignore_patterns("spec*.json", "synthetic-manifest.json"))
    positions_file = os.path.join(bad_orientation_dir, "positions.json")
    positions_doc = {
        "schema": POSITIONS_SCHEMA,
        "positions": [
            {"label": "D%d" % (index + 1), "role": "design", "weight": 1.0, "mic_orientation": "0deg"}
            if index == 0
            else {"label": "D%d" % (index + 1), "role": "design", "weight": 1.0}
            for index in range(6)
        ]
        + [{"label": "V%d" % (index + 1), "role": "validation", "weight": 1.0} for index in range(3)],
    }
    write_text(positions_file, json.dumps(positions_doc, ensure_ascii=False))
    cases.append(
        dict(
            name="build-bad-orientation-rejected",
            **_run_tool(["build", "--input", bad_orientation_dir, "--out", os.path.join(bad_orientation_dir, "spec.json"), "--positions", positions_file], 3, E_POSITIONS),
        )
    )

    flat_dir = os.path.join(out_dir, "flat-layout")
    os.makedirs(flat_dir, exist_ok=True)
    for label in ["D1", "D2", "D3", "D4", "D5", "D6", "V1", "V2", "V3"]:
        for channel in ["FL", "FR"]:
            shutil.copyfile(os.path.join(fixture_dir, label, "%s.json" % channel), os.path.join(flat_dir, "%s-%s.analysis.json" % (label, channel)))
            shutil.copyfile(os.path.join(fixture_dir, label, "%s.diag.json" % channel), os.path.join(flat_dir, "%s-%s.diag.json" % (label, channel)))
    cases.append(dict(name="build-flat-layout", **_run_tool(["build", "--input", flat_dir, "--out", os.path.join(flat_dir, "spec.json")], 0)))

    legacy_dir = os.path.join(out_dir, "legacy-naming")
    os.makedirs(legacy_dir, exist_ok=True)
    for label in ["D1", "D2", "D3", "D4", "D5", "D6", "V1", "V2", "V3"]:
        os.makedirs(os.path.join(legacy_dir, label), exist_ok=True)
        for channel_index, channel in enumerate(["FL", "FR"]):
            shutil.copyfile(
                os.path.join(fixture_dir, label, "%s.json" % channel),
                os.path.join(legacy_dir, label, "%s-ch%02d.analysis.json" % (channel, channel_index)),
            )
            shutil.copyfile(
                os.path.join(fixture_dir, label, "%s.diag.json" % channel),
                os.path.join(legacy_dir, label, "%s-ch%02d.diag.json" % (channel, channel_index)),
            )
    cases.append(dict(name="build-legacy-measure-multi-naming", **_run_tool(["build", "--input", legacy_dir, "--out", os.path.join(legacy_dir, "spec.json")], 0)))

    positions_ok = os.path.join(out_dir, "positions-config", "positions.json")
    os.makedirs(os.path.dirname(positions_ok), exist_ok=True)
    weights = [1.0, 0.9, 0.8, 0.8, 0.7, 0.7]
    positions_ok_doc = {
        "schema": POSITIONS_SCHEMA,
        "positions": [
            {"label": "D%d" % (index + 1), "role": "design", "weight": weights[index], "mic_orientation": "90deg"} for index in range(6)
        ]
        + [{"label": "V%d" % (index + 1), "role": "validation", "weight": 1.0, "mic_orientation": "90deg"} for index in range(3)],
    }
    write_text(positions_ok, json.dumps(positions_ok_doc, ensure_ascii=False))
    spec_cfg = os.path.join(out_dir, "positions-config", "spec.json")
    cases.append(dict(name="build-positions-config", **_run_tool(["build", "--input", fixture_dir, "--out", spec_cfg, "--positions", positions_ok], 0)))
    if cases[-1]["pass"]:
        spec_doc = read_json(spec_cfg)
        applied = [item["weight"] for item in spec_doc["design_positions"]]
        cases.append({"name": "positions-weights-applied", "pass": applied == weights, "detail": "weights=%s" % applied})

    passed = sum(1 for case in cases if case.get("pass"))
    result = {
        "schema": "tspi-cal-multipoint-spec-selftest/v1",
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION},
        "seed": seed,
        "out": out_dir,
        "cases": cases,
        "passed": passed,
        "total": len(cases),
        "ok": passed == len(cases),
        "note": "工具自测（合成数据/SIMULATED）；不代表真实测量/T6 验收",
    }
    write_text(os.path.join(out_dir, "SELFTEST.json"), json_text(result))
    if not result["ok"]:
        failed = [case["name"] for case in cases if not case.get("pass")]
        raise SpecError(E_RUN, "selftest 失败 %d/%d：%s" % (len(cases) - passed, len(cases), ", ".join(failed)))
    return {"command": "selftest", "out": out_dir, "passed": passed, "total": len(cases), "ok": True, "result": os.path.join(out_dir, "SELFTEST.json")}


# ---------------------------------------------------------------------------
# verify-e2e：断言离线全链证据
# ---------------------------------------------------------------------------
def _make_check(checks, check_id, description, expected, actual, ok, detail=None):
    checks.append({"id": check_id, "description": description, "expected": expected, "actual": actual, "pass": bool(ok), "detail": detail})


def cmd_verify_e2e(args):
    run_dir = os.path.abspath(args.run)
    summary_path = os.path.join(run_dir, "summary.json")
    job_path = os.path.join(run_dir, "job.json")
    require(os.path.isfile(summary_path), E_RUN, "缺 run/summary.json：%s" % run_dir)
    require(os.path.isfile(job_path), E_RUN, "缺 run/job.json：%s" % run_dir)
    summary = read_json(summary_path)
    job = read_json(job_path)
    checks = []
    expect_design = int(args.expect_design)
    expect_validation = int(args.expect_validation)

    measured_raw = summary.get("measured_design")
    if isinstance(measured_raw, str):
        measured = json.loads(measured_raw)
    elif isinstance(measured_raw, dict):
        measured = measured_raw
    else:
        measured = {}
    multipoint = measured.get("multipoint", {})
    design_count = multipoint.get("design_count")
    validation_count = multipoint.get("validation_count")
    _make_check(checks, "plan.design_count", "measured 构建的 T63 plan design 点数", expect_design, design_count, design_count == expect_design)
    _make_check(checks, "plan.validation_count", "measured 构建的 T63 plan validation/holdout 点数", expect_validation, validation_count, validation_count == expect_validation)
    fit_agg = multipoint.get("fit_aggregation", {})
    _make_check(
        checks,
        "fit.design_only",
        "T64 只聚合 design 位置（validation 不参与拟合）",
        {"design_only": True, "validation_in_fit": False},
        {"design_only": fit_agg.get("design_only"), "validation_in_fit": fit_agg.get("validation_in_fit")},
        fit_agg.get("design_only") is True and fit_agg.get("validation_in_fit") is False,
    )
    t6 = multipoint.get("t6_minimums", {})
    _make_check(
        checks,
        "t6.points_met",
        "T6 最低点数 6+3 达标（仅点数；不等于 T6 验收）",
        "POINTS_MET_NOT_ACCEPTED",
        t6.get("status"),
        t6.get("points_met") is True and t6.get("status") == "POINTS_MET_NOT_ACCEPTED",
    )
    holdout = multipoint.get("holdout", {})
    _make_check(
        checks,
        "holdout.predicted_only",
        "holdout 只做 measured-at-validation 的 predicted 评估（remeasure NOT_RUN、不参与拟合）",
        {"participates_in_fit": False, "remeasure_after_apply": "NOT_RUN"},
        {"participates_in_fit": holdout.get("participates_in_fit"), "remeasure_after_apply": holdout.get("remeasure_after_apply"), "evaluation": holdout.get("evaluation")},
        holdout.get("participates_in_fit") is False and holdout.get("remeasure_after_apply") == "NOT_RUN",
    )

    _make_check(checks, "job.state", "job 状态", "complete", job.get("state"), job.get("state") == "complete")
    _make_check(checks, "job.applied_version", "job applied_version 非空", "non-null", job.get("applied_version"), bool(job.get("applied_version")))
    host_path = os.path.join(run_dir, "host-apply", "host-report.json")
    require(os.path.isfile(host_path), E_RUN, "缺 host-apply/host-report.json：%s" % run_dir)
    host = read_json(host_path)
    plan = host.get("plan", {})
    alsa = host.get("alsa", {})
    _make_check(checks, "host.status", "tspi-rt-host plan-arena 状态", "ok", host.get("status"), host.get("status") == "ok")
    _make_check(checks, "host.plan_parse", "plan 解析", "t20-ok", plan.get("parse"), plan.get("parse") == "t20-ok")
    _make_check(checks, "host.manifest", "plan manifest 校验", "verified", plan.get("manifest"), plan.get("manifest") == "verified")
    swap = host.get("swap", {})
    blocks = host.get("blocks")
    _make_check(checks, "host.blocks", "host 运行 block 数 > 0", ">0", blocks, isinstance(blocks, int) and blocks > 0)
    acks = host.get("acks", swap.get("acks"))
    all_ack_blocks = host.get("all_ack_blocks", swap.get("all_ack_blocks"))
    retire_ready = host.get("retire_ready", swap.get("retire_ready"))
    _make_check(checks, "host.acks", "swap ack 数 > 0", ">0", acks, isinstance(acks, int) and acks > 0)
    _make_check(checks, "host.all_ack_blocks", "all-ack blocks 等于运行 blocks", blocks, all_ack_blocks, blocks is not None and all_ack_blocks == blocks)
    _make_check(checks, "host.retire_ready", "plan retire_ready", True, retire_ready, retire_ready is True)
    _make_check(checks, "host.backend_fake", "host backend=fake（离线，无真实声卡）", "fake", alsa.get("backend"), alsa.get("backend") == "fake")
    ack_path = os.path.join(run_dir, "jobs", "job-0001", "apply", "apply-ack.json")
    require(os.path.isfile(ack_path), E_RUN, "缺 apply-ack.json：%s" % run_dir)
    ack = read_json(ack_path)
    _make_check(checks, "apply.applied", "T66 apply applied=true", True, ack.get("applied"), ack.get("applied") is True)
    _make_check(checks, "apply.overall_status", "apply 结果口径（T3 未实测确认）", "APPLIED_NOT_CONFIRMED", ack.get("overall_status"), ack.get("overall_status") == "APPLIED_NOT_CONFIRMED")
    _make_check(checks, "apply.t3_confirmed", "T3 未确认（不得写 CONFIRMED）", False, ack.get("t3_confirmed"), ack.get("t3_confirmed") is False)

    experiment = summary.get("experiment", {})
    _make_check(
        checks,
        "experiment.scope",
        "实验产物口径",
        "MULTIPOINT_PILOT_NOT_CERTIFIABLE / NON_ACCEPTABLE",
        {"scope": experiment.get("scope"), "acceptability": experiment.get("acceptability"), "verdict": experiment.get("verdict")},
        experiment.get("scope") == "MULTIPOINT_PILOT_NOT_CERTIFIABLE"
        and experiment.get("acceptability") == "NON_ACCEPTABLE"
        and experiment.get("verdict") == "EXPERIMENT",
    )

    rt60_info = summary.get("rt60", {})
    artifacts = rt60_info.get("artifacts", [])
    text_artifacts = [item for item in artifacts if not item.endswith((".wav", ".raw"))]
    missing_files = []
    for relative in text_artifacts:
        if not os.path.isfile(os.path.join(run_dir, "jobs", "job-0001", relative)):
            missing_files.append(relative)
    expected_sets = 0
    for stem in ("remeasure/rt60/summary.json", "holdout/rt60/summary.json", "rt60/summary.json"):
        if os.path.isfile(os.path.join(run_dir, "jobs", "job-0001", stem)):
            expected_sets += 1
    _make_check(
        checks,
        "rt60.artifacts",
        "RT60/CSD 产物齐全（capture/remeasure/holdout 三套 summary+CSV+CSD）",
        {"sets": 3, "missing": []},
        {"sets": expected_sets, "missing": missing_files, "artifacts": len(artifacts)},
        expected_sets == 3 and not missing_files,
    )
    _make_check(
        checks,
        "rt60.not_real_room",
        "RT60 来源为 fake 引擎采集（真实房间测量 NOT_RUN）",
        "NOT_RUN / simulated",
        rt60_info.get("real_room_measurement"),
        isinstance(rt60_info.get("real_room_measurement"), str) and "NOT_RUN" in rt60_info.get("real_room_measurement"),
    )

    # 4b) 合成包标注（SIMULATED provenance / manifest / 点数）
    report_numbers = {}
    if args.package:
        package_dir = os.path.abspath(args.package)
        synth_manifest_path = os.path.join(package_dir, "synthetic-manifest.json")
        spec_path = os.path.join(package_dir, "spec.json")
        spec_manifest_path = os.path.join(package_dir, "spec-manifest.json")
        require(os.path.isfile(synth_manifest_path), E_RUN, "缺 package/synthetic-manifest.json：%s" % package_dir)
        require(os.path.isfile(spec_path), E_RUN, "缺 package/spec.json：%s" % package_dir)
        synth_manifest = read_json(synth_manifest_path)
        spec_doc = read_json(spec_path)
        spec_sha = sha256_bytes(open(spec_path, "rb").read())
        spec_manifest = read_json(spec_manifest_path) if os.path.isfile(spec_manifest_path) else {}
        provenance = str(spec_doc.get("provenance", ""))
        _make_check(
            checks,
            "package.simulated_marker",
            "合成输入必须标 SIMULATED（synthetic-manifest.simulated=true 且 spec.provenance 含 simulated）",
            {"manifest.simulated": True, "provenance": "contains simulated"},
            {"manifest.simulated": synth_manifest.get("simulated"), "provenance": provenance},
            synth_manifest.get("simulated") is True and "simulat" in provenance.lower(),
        )
        _make_check(
            checks,
            "package.spec_manifest_sha",
            "spec-manifest.spec_sha256 与 spec.json 文件字节一致",
            spec_sha,
            spec_manifest.get("spec_sha256"),
            spec_manifest.get("spec_sha256") == spec_sha,
        )
        package_design = len(spec_doc.get("design_positions", []))
        package_validation = len(spec_doc.get("validation_positions", []))
        _make_check(
            checks,
            "package.points",
            "spec v2 点数（design/validation）",
            {"design": expect_design, "validation": expect_validation},
            {"design": package_design, "validation": package_validation},
            package_design == expect_design and package_validation == expect_validation,
        )
        report_numbers["package"] = {
            "dir": package_dir,
            "spec_sha256": spec_sha,
            "simulated": synth_manifest.get("simulated"),
            "provenance": provenance,
            "design": package_design,
            "validation": package_validation,
        }

    report_dir = os.path.abspath(args.report) if args.report else None
    base_mp = None
    if report_dir:
        mp_report_path = os.path.join(report_dir, "multipoint-report.json")
        require(os.path.isfile(mp_report_path), E_RUN, "缺 report/multipoint-report.json：%s" % report_dir)
        mp_report = read_json(mp_report_path)
        base_mp = mp_report
        counts = mp_report.get("counts", {})
        report_numbers["counts"] = counts
        _make_check(
            checks,
            "report.counts",
            "多点证据 counts（design/validation/total）",
            {"design": expect_design, "validation": expect_validation, "total": expect_design + expect_validation},
            counts,
            counts == {"design": expect_design, "validation": expect_validation, "total": expect_design + expect_validation},
        )
        channels = mp_report.get("channels", [])
        residuals = []
        for channel in channels:
            for item in channel.get("per_position_residuals", []):
                residuals.append(item)
        roles = sorted({item.get("role") for item in residuals})
        _make_check(
            checks,
            "report.per_position_residuals",
            "逐位置残差覆盖 design+validation 全部位置（每通道）",
            {"channels": len(channels), "per_channel": expect_design + expect_validation, "roles": ["design", "validation"]},
            {"channels": len(channels), "per_channel_max": max((len(c.get("per_position_residuals", [])) for c in channels), default=0), "roles": roles},
            channels
            and all(len(c.get("per_position_residuals", [])) == expect_design + expect_validation for c in channels)
            and roles == ["design", "validation"],
        )
        seat_spread = mp_report.get("seat_spread", [])
        _make_check(
            checks,
            "report.seat_spread",
            "座位间 spread（逐通道 max/rms/worst_freq）",
            {"channels": len(channels)},
            {"entries": len(seat_spread)},
            len(seat_spread) == len(channels) and len(channels) > 0,
        )
        holdout_report = mp_report.get("holdout", {})
        _make_check(
            checks,
            "report.holdout",
            "holdout 预测口径（participates_in_fit=false / remeasure NOT_RUN）",
            {"participates_in_fit": False, "remeasure_after_apply": "NOT_RUN"},
            holdout_report,
            holdout_report.get("participates_in_fit") is False
            and holdout_report.get("remeasure_after_apply") == "NOT_RUN"
            and holdout_report.get("evaluation") == "PREDICTED_FROM_MEASURED_CURVES_AT_VALIDATION_POSITIONS",
        )
        report_numbers["residuals"] = residuals
        report_numbers["seat_spread"] = seat_spread
        fitp_path = os.path.join(report_dir, "fitp-summary.json")
        if os.path.isfile(fitp_path):
            fitp = read_json(fitp_path)
            channels_expected = len(channels)
            _make_check(
                checks,
                "report.fitp",
                "probe FitP 计数（curves=全部位置×通道；phase_responses=仅 design×通道）",
                {"design": expect_design, "validation": expect_validation, "curves": (expect_design + expect_validation) * channels_expected, "phase": expect_design * channels_expected},
                {"design": fitp.get("design_positions"), "validation": fitp.get("validation_positions"), "curves": fitp.get("curves"), "phase": fitp.get("phase_responses")},
                fitp.get("design_positions") == expect_design
                and fitp.get("validation_positions") == expect_validation
                and fitp.get("curves") == (expect_design + expect_validation) * channels_expected
                and fitp.get("phase_responses") == expect_design * channels_expected,
            )
            report_numbers["p_sha256"] = fitp.get("p_sha256")
            report_numbers["measurement_sha256"] = fitp.get("measurement_sha256")

    if args.rt60_by_position:
        rt60_dir = os.path.abspath(args.rt60_by_position)
        rt60_summary_path = os.path.join(rt60_dir, "summary.json")
        require(os.path.isfile(rt60_summary_path), E_RUN, "缺逐位置 rt60/summary.json：%s" % rt60_dir)
        rt60_summary = read_json(rt60_summary_path)
        rows = rt60_summary.get("rows", [])
        labels = sorted({row.get("label") for row in rows})
        position_expected = expect_design + expect_validation
        if base_mp is not None:
            positions = [item["label"] for item in base_mp.get("positions", [])]
            channels = [item.get("name") for item in base_mp.get("channels", [])]
            expected_labels = sorted("%s_%s" % (position, channel) for position in positions for channel in channels)
        else:
            expected_labels = labels
        _make_check(
            checks,
            "rt60_by_position.rows",
            "逐位置 RT60 行覆盖全部位置×通道（label=<position>_<channel>）",
            {"labels": len(expected_labels), "rows": len(expected_labels)},
            {"labels": len(labels), "rows": len(rows)},
            len(labels) == len(expected_labels) and len(rows) >= len(labels),
        )
        csd_dir = os.path.join(rt60_dir, "csd")
        csd_files = [name for name in os.listdir(csd_dir)] if os.path.isdir(csd_dir) else []
        _make_check(checks, "rt60_by_position.csd", "逐位置 CSD JSON 产物", len(labels), len(csd_files), len(csd_files) == len(labels))
        report_numbers["rt60_by_position"] = {"labels": labels, "rows": len(rows), "csd": len(csd_files)}

    if args.perturbed_run:
        perturbed_dir = os.path.abspath(args.perturbed_run)
        perturbed_summary_path = os.path.join(perturbed_dir, "summary.json")
        require(os.path.isfile(perturbed_summary_path), E_RUN, "缺 perturbed run/summary.json：%s" % perturbed_dir)
        perturbed = read_json(perturbed_summary_path)
        base_plan = (summary.get("compile_plan") or {}).get("sha256")
        perturbed_plan = (perturbed.get("compile_plan") or {}).get("sha256")
        base_measured = json.loads(summary["measured_design"]) if isinstance(summary.get("measured_design"), str) else summary.get("measured_design", {})
        perturbed_measured = json.loads(perturbed["measured_design"]) if isinstance(perturbed.get("measured_design"), str) else perturbed.get("measured_design", {})
        _make_check(
            checks,
            "holdout.plan_unchanged",
            "扰动 validation 数据后 plan.bin sha256 不变（validation 不参与 fit/compile）",
            base_plan,
            perturbed_plan,
            bool(base_plan) and base_plan == perturbed_plan,
        )
        _make_check(
            checks,
            "holdout.measurement_digest_changed",
            "扰动 validation 数据后 measurement digest 改变（数据确被读入，只是不进拟合）",
            "different",
            {"base": base_measured.get("measurement_sha256"), "perturbed": perturbed_measured.get("measurement_sha256")},
            base_measured.get("measurement_sha256") != perturbed_measured.get("measurement_sha256"),
        )
        if args.perturbed_report:
            if not report_dir:
                raise SpecError(E_USAGE, "--perturbed-report 需要同时给 --report（原始 probe report 目录）")
            base_mp = read_json(os.path.join(report_dir, "multipoint-report.json"))
            pert_mp = read_json(os.path.join(os.path.abspath(args.perturbed_report), "multipoint-report.json"))
            base_fitp_path = os.path.join(report_dir, "fitp-summary.json")
            pert_fitp_path = os.path.join(os.path.abspath(args.perturbed_report), "fitp-summary.json")
            require(os.path.isfile(base_fitp_path) and os.path.isfile(pert_fitp_path), E_RUN, "holdout 对照缺 probe fitp-summary.json")
            base_fitp = read_json(base_fitp_path)
            pert_fitp = read_json(pert_fitp_path)
            design_labels = [item["label"] for item in base_mp.get("positions", []) if item.get("role") == "design"]
            validation_labels = [item["label"] for item in base_mp.get("positions", []) if item.get("role") == "validation"]
            design_identical = True
            holdout_changed_all = True
            detail_rows = []
            for base_channel, pert_channel in zip(base_mp.get("channels", []), pert_mp.get("channels", [])):
                base_map = {item["label"]: item for item in base_channel.get("per_position_residuals", [])}
                pert_map = {item["label"]: item for item in pert_channel.get("per_position_residuals", [])}
                for label in design_labels:
                    if base_map.get(label) != pert_map.get(label):
                        design_identical = False
                changed = any(base_map.get(label) != pert_map.get(label) for label in validation_labels)
                if not changed:
                    holdout_changed_all = False
                detail_rows.append(
                    {
                        "channel": base_channel.get("name"),
                        "design_identical": all(base_map.get(label) == pert_map.get(label) for label in design_labels),
                        "holdout_changed": changed,
                        "holdout_after_p_base": [base_map.get(label, {}).get("modal_rms_after_p_db") for label in validation_labels],
                        "holdout_after_p_perturbed": [pert_map.get(label, {}).get("modal_rms_after_p_db") for label in validation_labels],
                    }
                )
            _make_check(
                checks,
                "holdout.report_p_sha_unchanged",
                "扰动 validation 后 probe p_sha256 不变（T64 P 只由 design 决定）",
                base_fitp.get("p_sha256"),
                pert_fitp.get("p_sha256"),
                bool(base_fitp.get("p_sha256")) and base_fitp.get("p_sha256") == pert_fitp.get("p_sha256"),
            )
            _make_check(
                checks,
                "holdout.report_design_residuals_identical",
                "扰动 validation 后 design 逐位置残差逐字段不变",
                True,
                design_identical,
                design_identical,
                detail=detail_rows,
            )
            _make_check(
                checks,
                "holdout.report_holdout_residuals_changed",
                "扰动 validation 后 validation 逐位置残差变化（数据进入 holdout 预测评估）",
                True,
                holdout_changed_all,
                holdout_changed_all,
                detail=detail_rows,
            )
            report_numbers["holdout_isolation"] = {
                "design_labels": design_labels,
                "validation_labels": validation_labels,
                "channels": detail_rows,
                "p_sha256": base_fitp.get("p_sha256"),
            }
        report_numbers["perturbed_plan_sha256"] = perturbed_plan

    passed = sum(1 for check in checks if check["pass"])
    result = {
        "schema": VERIFY_SCHEMA,
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION},
        "run": run_dir,
        "report": report_dir,
        "perturbed_run": os.path.abspath(args.perturbed_run) if args.perturbed_run else None,
        "checks": checks,
        "passed": passed,
        "total": len(checks),
        "ok": passed == len(checks),
        "numbers": report_numbers,
        "note": "离线 fake 全链校验；EXPERIMENT/NON_ACCEPTABLE；真实多点测量/T3/T6/T68 仍 NOT_RUN",
    }
    write_text(args.out, json_text(result))
    if not result["ok"]:
        failed = [check["id"] for check in checks if not check["pass"]]
        raise SpecError(E_RUN, "verify-e2e 失败 %d/%d：%s" % (len(checks) - passed, len(checks), ", ".join(failed)))
    return {"command": "verify-e2e", "out": os.path.abspath(args.out), "passed": passed, "total": len(checks), "ok": True}


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser():
    parser = argparse.ArgumentParser(
        prog="make-multipoint-spec",
        description="多点 measured spec v2 离线生成/校验工具（纯离线、不出声；合成数据一律 SIMULATED）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "示例：\n"
            "  python make_multipoint_spec.py synthetic --out PKG --design 6 --validation 3\n"
            "  python make_multipoint_spec.py build --input PKG --out PKG/spec.json\n"
            "  python make_multipoint_spec.py check --spec PKG/spec.json --require-t6\n"
            "  python make_multipoint_spec.py selftest --out SELFTEST\n"
            "  python make_multipoint_spec.py verify-e2e --run RUN --report REPORT --out VERIFY.json\n"
        ),
    )
    parser.add_argument("--version", action="version", version="%s %s" % (TOOL_NAME, TOOL_VERSION))
    sub = parser.add_subparsers(dest="command", required=True)

    synth = sub.add_parser("synthetic", help="生成确定性合成 analysis/diag 夹具（SIMULATED）")
    synth.add_argument("--out", required=True, help="输出目录（会包含 D1..D6/V1..V3 子目录）")
    synth.add_argument("--design", type=int, default=6, help="design 位置数（默认 6）")
    synth.add_argument("--validation", type=int, default=3, help="validation 位置数（默认 3）")
    synth.add_argument("--channels", default="FL,FR", help="通道列表（默认 FL,FR）")
    synth.add_argument("--seed", default="multipoint-prep-20261122", help="确定性 seed（写进 manifest）")
    synth.add_argument("--rate", type=int, default=DEFAULT_RATE, help="采样率（默认 48000）")
    synth.add_argument("--force", action="store_true", help="覆盖已存在的非空输出目录")

    synth_ir = sub.add_parser("synthetic-ir", help="生成确定性合成 IR WAV（SIMULATED，逐位置/通道）")
    synth_ir.add_argument("--out", required=True, help="输出目录")
    synth_ir.add_argument("--design", type=int, default=6)
    synth_ir.add_argument("--validation", type=int, default=3)
    synth_ir.add_argument("--channels", default="FL,FR")
    synth_ir.add_argument("--seed", default="multipoint-prep-20261122")
    synth_ir.add_argument("--rate", type=int, default=DEFAULT_RATE)
    synth_ir.add_argument("--seconds", type=float, default=1.0, help="IR 长度秒（默认 1.0，覆盖 900 ms RT60 窗口）")
    synth_ir.add_argument("--force", action="store_true")

    build = sub.add_parser("build", help="analysis 目录 -> spec v2 + manifest")
    build.add_argument("--input", required=True, help="按位置命名的 analysis JSON 根目录")
    build.add_argument("--out", required=True, help="输出 spec v2 JSON 路径")
    build.add_argument("--manifest", default=None, help="输出 manifest 路径（默认 <out 去扩展名>.manifest.json）")
    build.add_argument("--positions", default=None, help="窗口前冻结的测点表 JSON（label/role/weight/mic_orientation）")
    build.add_argument("--template", default=None, help="已有 spec v2（复制 roles/dense_to_dac/compile 等接线字段）")
    build.add_argument("--session-id", default="multipoint-measured", help="spec.session_id（建议 <window>-<layout>）")
    build.add_argument("--layout", default=None, help="layout（默认取 template 或 stereo-2.0）")
    build.add_argument("--target", default=None, help="target flat|tilt10（默认取 template 或 flat）")
    build.add_argument("--provenance", default="UNSPECIFIED_MUST_EDIT", help="数据来源声明（真实测量必须写清；synthetic 会标 SIMULATED）")
    build.add_argument("--sample-rate", type=int, default=DEFAULT_RATE)
    build.add_argument("--channels", default=None, help="规范通道顺序（默认由输入推断）")
    build.add_argument("--design-labels", default=None, help="逗号分隔 design label（覆盖 D*/V* 约定）")
    build.add_argument("--validation-labels", default=None, help="逗号分隔 validation label")
    build.add_argument("--layout-mode", choices=("auto", "tree", "flat"), default="auto")
    build.add_argument("--allow-below-t6", action="store_true", help="允许 design<6/validation<3（pilot；会记 warning）")
    build.add_argument("--min-snr-db", type=float, default=MIN_SNR_DB_DEFAULT, help="SNR 门槛（默认 25 dB；降低会记 warning）")
    build.add_argument("--max-clock-ppm-spread", type=float, default=MAX_CLOCK_PPM_SPREAD_DEFAULT, help="位置间 clock ppm 允许跨度（默认 5）")
    build.add_argument("--min-repeats", type=int, default=MIN_REPEATS_DEFAULT, help="每位置最少重复遍数（默认 3）")
    build.add_argument("--allow-missing-diag", action="store_true", help="允许缺 diag（clock/repeats 门不可判定；记 warning）")
    build.add_argument("--simulated", action="store_true", help="强制 manifest 标 SIMULATED")
    build.add_argument("--stamp", default=None, help="可选：写入固定 generated_utc 字符串（默认不写，保证字节稳定）")
    build.add_argument("--quiet", action="store_true")

    check = sub.add_parser("check", help="校验已有 spec v2 + 文件/质量门")
    check.add_argument("--spec", required=True)
    check.add_argument("--require-t6", action="store_true", help="要求 design>=6/validation>=3")
    check.add_argument("--min-snr-db", type=float, default=MIN_SNR_DB_DEFAULT)
    check.add_argument("--min-repeats", type=int, default=MIN_REPEATS_DEFAULT)
    check.add_argument("--allow-missing-diag", action="store_true")
    check.add_argument("--json", action="store_true", help="stdout 输出 JSON 摘要")
    check.add_argument("--out", default=None, help="可选：把校验摘要写入 JSON 文件")

    selftest = sub.add_parser("selftest", help="工具自测（正向/负向/确定性）")
    selftest.add_argument("--out", required=True)
    selftest.add_argument("--force", action="store_true")

    verify = sub.add_parser("verify-e2e", help="断言离线全链证据（run/report/perturbed）")
    verify.add_argument("--run", required=True, help="tspi-cal run 输出目录")
    verify.add_argument("--report", default=None, help="multipoint_probe report 输出目录")
    verify.add_argument("--package", default=None, help="合成/交接包目录（spec.json + spec-manifest.json + synthetic-manifest.json）")
    verify.add_argument("--perturbed-run", default=None, help="扰动 validation 数据后的第二次 run 目录（holdout 隔离对照）")
    verify.add_argument("--perturbed-report", default=None, help="扰动 validation 数据后的 multipoint_probe report 目录")
    verify.add_argument("--rt60-by-position", default=None, help="tspi-cal rt60 逐位置输出目录")
    verify.add_argument("--expect-design", type=int, default=6)
    verify.add_argument("--expect-validation", type=int, default=3)
    verify.add_argument("--out", default="VERIFY.json")

    return parser


def _default_manifest(out_path):
    base, _ext = os.path.splitext(out_path)
    return base + ".manifest.json"


def main(argv=None):
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "synthetic":
            channels = [item for item in args.channels.split(",") if item]
            require(args.design <= MAX_DESIGN and args.validation <= MAX_VALIDATION, E_USAGE, "design/validation 数越界")
            result = synth_package(args.out, int(args.design), int(args.validation), channels, args.seed, int(args.rate), args.force)
        elif args.command == "synthetic-ir":
            channels = [item for item in args.channels.split(",") if item]
            require(args.design <= MAX_DESIGN and args.validation <= MAX_VALIDATION, E_USAGE, "design/validation 数越界")
            result = synth_ir(args.out, int(args.design), int(args.validation), channels, args.seed, int(args.rate), float(args.seconds), args.force)
        elif args.command == "build":
            if args.manifest is None:
                args.manifest = _default_manifest(args.out)
            result = build_spec(args)
        elif args.command == "check":
            spec_doc = check_spec(args.spec, args.require_t6, args.min_snr_db, args.min_repeats, args.allow_missing_diag)
            result = {
                "command": "check",
                "spec": spec_doc["spec"],
                "design": spec_doc["design"],
                "validation": spec_doc["validation"],
                "t6_points_met": spec_doc["t6_points_met"],
                "warnings": spec_doc["checks"]["warnings"],
                "ok": True,
            }
            if args.out:
                write_text(args.out, json_text(spec_doc))
        elif args.command == "selftest":
            result = cmd_selftest(args)
        elif args.command == "verify-e2e":
            result = cmd_verify_e2e(args)
        else:
            raise SpecError(E_USAGE, "未知命令 %r" % args.command)
    except SpecError as exc:
        sys.stderr.write("ERROR [%s] %s\n" % (exc.code, exc.message))
        for detail in exc.details:
            sys.stderr.write("  - %s\n" % detail)
        return EXIT_OUTPUT if exc.code == E_OUTPUT else (EXIT_USAGE if exc.code == E_USAGE else EXIT_VALIDATION)
    if args.command == "check" and not getattr(args, "json", False):
        print("OK check: %s design=%d validation=%d t6=%s warnings=%d" % (result["spec"], result["design"], result["validation"], result["t6_points_met"], len(result["warnings"])))
    else:
        print(json.dumps(result, ensure_ascii=False))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
