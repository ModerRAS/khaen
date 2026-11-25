#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""khaen-calibration/v1 与 v1.1 契约校验器（纯 Python + PyYAML；板端可跑）。

v1.1（2026-11-16，裁决 5）：filters[].type = peaking|low_shelf|high_shelf。
  peaking 的 q 为有限数且 ≥0.1（无上限，窄模态可 >10）；
  shelf（low_shelf/high_shelf）的 q 是 RBJ slope S，必须在 [0.1, 1.0]
  （CamillaDSP slope=12*S 上限 12 dB/oct）。
v1 仍只允许 peaking 且 Q≤10。纯 Peaking、Q≤10 且无 error 的 v1.1 文件会被标注「v1 可读取」；
含架滤波或 peaking Q>10 时不标注（v1 消费方读不了，须按 §10 拒绝或降级说明）。

2026-11-17（B2，C-13/P2-8/P2-14）：delay_samples 方向按 CONTRACT §3.1（D_i=max_j t_j−t_i，不是“相对最早到达”）；
reference_index 应指向 delay=0 的通道（否则 warning）；v1.1 peaking q>10 提示仅 CamillaDSP 片段可应用；
若 measurement.playback.channels 给出，channels[].index 必须小于它。

2026-11-17（D7 裁决）：允许 ≤ +3 dB boost。单段 filters[].gain_db>0 超过 +3.0 dB 给 warning
（producer 硬帽；Rust 主入口补齐前不阻断）；每通道正向总和沿用 >6 dB warning / >12 dB error（总增益帽）。

用法：
  python3 tools/validate_calibration.py FILE [--layout-presets /etc/tspi-audio/layout-presets.yml]
                                   [--strict] [--json]
退出码：0 合法（可能有 warning）；1 不合法；2 用法/依赖错误。
"""
import argparse
import json
import math
import os
import re
import sys

try:
    import yaml
except ImportError:
    sys.stderr.write("需要 PyYAML（Debian: python3-yaml）\n")
    sys.exit(2)

SCHEMA = "khaen-calibration/v1"
SCHEMA_V11 = "khaen-calibration/v1.1"
SUPPORTED_SCHEMAS = (SCHEMA, SCHEMA_V11)
NAME_RE = re.compile(r"^[A-Z][A-Z0-9]*$")
TOP_KEYS = {"schema", "sample_rate", "reference_index", "layout", "created_unix",
            "tool", "version", "note", "measurement", "channels"}
CH_KEYS = {"name", "index", "delay_samples", "polarity", "gain_db", "filters", "measured"}
PEQ_KEYS = {"freq", "q", "gain_db", "type"}
MEAS_KEYS = {"mode", "sweep", "playback", "capture", "pre_seconds", "post_seconds",
             "repeats", "render_offsets"}


def is_num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(float(v))


def is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


class Check(object):
    def __init__(self):
        self.errors = []
        self.warnings = []

    def err(self, msg):
        self.errors.append(msg)

    def warn(self, msg):
        self.warnings.append(msg)


def load_yaml(path):
    with open(path, "r", encoding="utf-8") as f:
        return yaml.safe_load(f)


def check_doc(doc, layout_presets=None):
    c = Check()
    if not isinstance(doc, dict):
        c.err("顶层必须是 mapping")
        return c
    for k in doc:
        if k not in TOP_KEYS:
            c.warn("未知顶层字段：%s" % k)
    schema = doc.get("schema")
    v11 = schema == SCHEMA_V11
    if schema not in SUPPORTED_SCHEMAS:
        c.err("schema 必须是 %r 或 %r（当前 %r）" % (SCHEMA, SCHEMA_V11, schema))
    fs = doc.get("sample_rate")
    if not is_int(fs):
        c.err("sample_rate 必须是整数")
        fs = 48000
    elif fs not in (44100, 48000, 88200, 96000):
        c.warn("sample_rate=%s 不是部署口径（44100/48000/88200/96000）" % fs)
    ref = doc.get("reference_index")
    if not is_int(ref) or ref < 0:
        c.err("reference_index 必须是非负整数")
    layout = doc.get("layout")
    if layout is not None and not isinstance(layout, str):
        c.err("layout 必须是字符串")
    if doc.get("created_unix") is not None and not is_int(doc.get("created_unix")):
        c.err("created_unix 必须是整数")
    tool = doc.get("tool")
    if tool is not None:
        if isinstance(tool, str):
            pass  # 兼容：字符串 = 工具名（version 可放顶层）
        elif isinstance(tool, dict):
            if "name" not in tool:
                c.err("tool 映射必须含 name")
        else:
            c.err("tool 必须是字符串或 {name,version}")
    if doc.get("version") is not None and not isinstance(doc.get("version"), (str, int, float)):
        c.err("version 必须是字符串/数字")

    meas = doc.get("measurement")
    playback_channels = None
    if meas is not None:
        if not isinstance(meas, dict):
            c.err("measurement 必须是 mapping")
        else:
            for k in meas:
                if k not in MEAS_KEYS:
                    c.warn("measurement 未知字段：%s" % k)
            playback = meas.get("playback")
            if isinstance(playback, dict) and is_int(playback.get("channels")):
                playback_channels = playback["channels"]
            mode = meas.get("mode")
            if mode not in (None, "direct", "through_render"):
                c.err("measurement.mode 必须是 direct|through_render")
            if mode == "through_render":
                ro = meas.get("render_offsets")
                if not isinstance(ro, list) or not ro:
                    c.err("mode=through_render 时必须提供非空 render_offsets")
                else:
                    ch_names = set()
                    chs = doc.get("channels")
                    if isinstance(chs, list):
                        for ch in chs:
                            if isinstance(ch, dict) and ch.get("name"):
                                ch_names.add(str(ch["name"]))
                    for item in ro:
                        if not isinstance(item, dict) or "name" not in item:
                            c.err("render_offsets 项必须是 {name,delay_samples,polarity}")
                        elif ch_names and str(item["name"]) not in ch_names:
                            c.warn("render_offsets.name=%s 不在 channels 中" % item["name"])

    v1_compatible = v11
    channels = doc.get("channels")
    if not isinstance(channels, list) or not channels:
        c.err("channels 必须是非空数组")
        return c
    if len(channels) > 32:
        c.err("channels 超过 32 条")

    names, indexes = set(), set()
    delays = []
    layout_roles = None
    if layout and layout_presets and os.path.isfile(layout_presets):
        try:
            lp = load_yaml(layout_presets)
            lay = ((lp or {}).get("output_layouts") or {}).get(layout)
            if not isinstance(lay, dict) or not isinstance(lay.get("roles"), list):
                c.warn("layout-presets 中没有 layout=%s，跳过映射交叉校验" % layout)
            else:
                layout_roles = {}
                for r in lay["roles"]:
                    if not isinstance(r, dict) or not r.get("id"):
                        continue
                    ov = r.get("outputs")
                    if isinstance(ov, int):
                        ov = [ov]
                    layout_roles[str(r["id"])] = [int(x) for x in (ov or []) if is_num(x)]
        except Exception as e:  # noqa: BLE001
            c.warn("读取 layout-presets 失败：%s" % e)

    for i, ch in enumerate(channels):
        p = "channels[%d]" % i
        if not isinstance(ch, dict):
            c.err(p + " 必须是 mapping")
            continue
        for k in ch:
            if k not in CH_KEYS:
                c.warn(p + " 未知字段：" + str(k))
        name = ch.get("name")
        if not isinstance(name, str) or not NAME_RE.match(name):
            c.err(p + ".name 必须是角色 id（大写字母/数字，如 FL/SUB1）")
            name = None
        elif name in names:
            c.err(p + ".name 重复：" + name)
        else:
            names.add(name)
        idx = ch.get("index")
        if not is_int(idx) or idx < 0:
            c.err(p + ".index 必须是非负整数")
        elif idx in indexes:
            c.err(p + ".index 重复：" + str(idx))
        else:
            indexes.add(idx)
            if playback_channels is not None and idx >= playback_channels:
                c.err(p + ".index=%d 超出 measurement.playback.channels=%d（index 是设备物理通道域）"
                      % (idx, playback_channels))
        d = ch.get("delay_samples")
        if not is_int(d) or d < 0:
            c.err(p + ".delay_samples 必须是非负整数（延迟补偿 D=max_j t_j - t_i，min=0 归一化）")
        else:
            delays.append((name or p, d))
            dmax = int(math.floor(0.2 * fs))
            if d > dmax:
                c.err(p + ".delay_samples=%d 超过 200ms 上限（%d samples @%d）" % (d, dmax, fs))
        pol = ch.get("polarity")
        if pol is None:
            c.warn(p + ".polarity 缺省（建议显式 normal）")
        elif pol in ("normal", "invert"):
            pass
        elif pol in (1, -1):
            c.warn(p + ".polarity 使用 ±1 别名（建议 normal|invert）")
        else:
            c.err(p + ".polarity 必须是 normal|invert（或兼容别名 1/-1）")
        g = ch.get("gain_db")
        if g is not None:
            if not is_num(g) or not (-12.0 <= float(g) <= 6.0):
                c.err(p + ".gain_db 必须在 [-12,+6]")
        filters = ch.get("filters")
        if not isinstance(filters, list):
            c.err(p + ".filters 必须是数组（可为空）")
            filters = []
        if len(filters) > 32:
            c.err(p + ".filters 超过 32 段")
        pos_boost = 0.0
        fmax = min(20000.0, fs / 2.0 - 1.0)
        for j, f in enumerate(filters[:32]):
            pp = p + ".filters[%d]" % j
            if not isinstance(f, dict):
                c.err(pp + " 必须是 mapping")
                continue
            for k in f:
                if k not in PEQ_KEYS:
                    c.warn(pp + " 未知字段：" + str(k))
            fq, q, gain = f.get("freq"), f.get("q"), f.get("gain_db")
            if not is_num(fq) or not (20.0 <= float(fq) <= fmax):
                c.err(pp + ".freq 必须在 [20, %.1f]" % fmax)
            ft = f.get("type")
            if ft is None:
                ft = "peaking"
            if ft not in ("peaking", "low_shelf", "high_shelf"):
                c.err(pp + ".type 不认识（当前 %r）" % (ft,))
                v1_compatible = False
            elif not v11 and ft != "peaking":
                c.err(pp + ".type v1 只允许 peaking（当前 %r）" % (ft,))
            elif ft != "peaking":
                v1_compatible = False
            # 裁决 5：v1.1 按 type 分流。peaking ≥0.1 无上限；shelf 的 q 是 RBJ slope S ∈ [0.1, 1.0]。
            shelf = ft in ("low_shelf", "high_shelf")
            if v11 and shelf:
                if not is_num(q) or not (0.1 <= float(q) <= 1.0):
                    c.err(pp + ".q shelf 的 q 是 RBJ slope S，必须在 [0.1,1.0]"
                          "（CamillaDSP slope=12q 上限 12 dB/oct）")
            elif v11:
                if not is_num(q) or float(q) < 0.1:
                    c.err(pp + ".q 必须是有限数且 ≥ 0.1")
                elif float(q) > 10.0:
                    # v1.1 允许窄模态 Q>10；但这样的文件不在 v1 可读取范围内
                    v1_compatible = False
                    c.warn(pp + ".q>10：契约合法（仅 CamillaDSP 片段路径可应用；"
                                  "channelmap 应用路线会拒绝整份文件）")
            elif not is_num(q) or not (0.1 <= float(q) <= 10.0):
                c.err(pp + ".q 必须在 [0.1,10]")
            if not is_num(gain) or not (-24.0 <= float(gain) <= 24.0):
                c.err(pp + ".gain_db 必须在 [-24,+24]")
            elif float(gain) > 0:
                if float(gain) > 3.0:
                    c.warn(pp + ".gain_db=+%.2f dB 超过 D7 单段 boost 帽 +3.0 dB"
                                  "（producer 应钳制；Rust 主入口补齐后转 error）" % float(gain))
                pos_boost += float(gain)
        if pos_boost > 12.0:
            c.err(p + " PEQ 正向提升总和 %.1f dB > 12 dB（需要负 trim/减少 boost）" % pos_boost)
        elif pos_boost > 6.0:
            c.warn(p + " PEQ 正向提升总和 %.1f dB > 6 dB（注意余量）" % pos_boost)
        if layout_roles is not None and name:
            if name not in layout_roles:
                c.err(p + ".name=%s 不在 layout=%s 的角色中" % (name, layout))
            elif is_int(idx) and layout_roles[name] and idx != layout_roles[name][0]:
                c.err(p + ".index=%s 与 layout 映射不一致（%s 期望 %s）"
                      % (idx, name, layout_roles[name][0]))
        meas_ch = ch.get("measured")
        if meas_ch is not None and not isinstance(meas_ch, dict):
            c.warn(p + ".measured 建议为 mapping（诊断用，可忽略）")

    # 只在文件本身合法时给提示：带 error 的文件即使结构上兼容也不应宣称「可直接读取」
    if v11 and v1_compatible and not c.errors:
        c.warn("v1.1 文件只含 Peaking 且 Q≤10：v1 消费方可直接读取")
    if delays:
        mn = min(d for _, d in delays)
        if mn != 0:
            c.err("delay_samples 未归一化：min=%d（必须为 0）" % mn)
        if is_int(ref) and ref in indexes:
            # 找出 reference_index 对应通道的 delay
            for ch in channels:
                if isinstance(ch, dict) and ch.get("index") == ref:
                    rd = ch.get("delay_samples")
                    if is_int(rd) and rd != 0:
                        c.warn("reference_index 指向的通道 delay_samples=%d（参考不是 delay=0/到达最晚的通道；"
                               "建议 reference_index 指向 D=0 的通道，见 CONTRACT §3.1）" % rd)
                    break
        if is_int(ref) and ref not in indexes:
            c.err("reference_index=%d 不在 channels[].index 中" % ref)
    else:
        if is_int(ref) and ref not in indexes:
            c.err("reference_index=%d 不在 channels[].index 中" % ref)

    # layout 角色覆盖提示（不强制：允许只校准部分通道）
    if layout_roles is not None and names:
        missing = sorted(set(layout_roles) - names)
        if missing:
            c.warn("layout=%s 有未校准角色：%s" % (layout, ", ".join(missing)))
    return c


def main():
    ap = argparse.ArgumentParser(description=(
        "khaen-calibration/v1 与 v1.1 校验器。"
        "v1：仅 peaking，q ∈ [0.1,10]。"
        "v1.1：peaking 的 q ≥ 0.1（无上限）；"
        "low_shelf/high_shelf 的 q 是 RBJ slope S，必须在 [0.1,1.0]。"
    ))
    ap.add_argument("file")
    ap.add_argument("--layout-presets", default=None)
    ap.add_argument("--strict", action="store_true", help="warning 视为失败")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    try:
        doc = load_yaml(args.file)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write("读取失败：%s\n" % e)
        return 2
    c = check_doc(doc, args.layout_presets)
    ok = not c.errors and (not args.strict or not c.warnings)
    out = {"schema": "khaen-calibration-validation/v1", "file": os.path.abspath(args.file),
           "ok": ok, "strict": bool(args.strict), "errors": c.errors, "warnings": c.warnings}
    if args.json:
        print(json.dumps(out, ensure_ascii=False, indent=2))
    else:
        print(("OK  " if ok else "FAIL") + "  " + args.file)
        for e in c.errors:
            print("  ERROR  " + e)
        for w in c.warnings:
            print("  WARN   " + w)
        print("结果：errors=%d warnings=%d%s"
              % (len(c.errors), len(c.warnings), "（strict）" if args.strict else ""))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
