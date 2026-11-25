#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把 khaen-calibration/v1 或 v1.1 应用到消费方配置（纯标准库 + PyYAML）。

不修改输入文件。退出码：0 成功（可有 WARN）；1 校验/合并失败；2 用法、依赖或读取错误。

模式 1 — channelmap v2 profile（板端 T3/T4 先走这条；输出可直接交给 khaen-channelmap apply）：

    python3 tools/apply_calibration.py \\
        --channelmap PROFILE.yml --calibration CAL.yml --out NEW.yml \\
        [--layout-role-map auto|FILE] [--backup-dir DIR] [--no-validate] [--dry-run]

按 CONTRACT §4.1，用 channels[].name 对齐 output.roles[].id（--layout-role-map 可改这个对应）：

    delay_ms = 原 delay_ms + delay_samples * 1000 / sample_rate     # 结果必须在 0..200
    polarity = XOR(原 polarity, 校准 polarity == invert)
    gain_db  = 原 gain_db + 校准 gain_db                            # 结果必须在 -60..+12
    peq      = 原 peq + [{freq, q, gain_db, type?}]                 # 总数 ≤ 32；peaking 可省略 type

默认接着跑 khaen-channelmap validate（--no-validate 关掉）。channelmap 的 peaking q 仍是
[0.1, 10]；v1.1 里 q>10 的窄模态过不了这条路线，工具不会静默丢段。

模式 2 — 把 producer 的 CamillaDSP 片段合并进一份完整配置：

    python3 tools/apply_calibration.py \\
        --camilladsp BASE.yml --fragment CAL.yml.camilladsp.yml --out NEW.yml \\
        [--camilladsp-bin PATH] [--dry-run]

filters 按名字并入（重名即失败）；fragment 的 pipeline 只允许 type=Filter，追加到 base 的 pipeline 末尾。
schema / note / channelmap_roles 不写入完整配置（CamillaDSP deny_unknown_fields）。
默认跑 camilladsp --check；可执行文件不在 PATH 且未给 --camilladsp-bin 时 WARN 并仍返回 0。
--check 失败则不留下 --out。

--dry-run 只把差异打到 stdout，不写 --out，也不写 --backup-dir。
"""
from __future__ import print_function

import argparse
import copy
import os
import shutil
import subprocess
import sys
import tempfile
import time

try:
    import yaml
except ImportError:
    sys.stderr.write("需要 PyYAML（Debian: python3-yaml）\n")
    sys.exit(2)

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.abspath(os.path.join(_HERE, "..", ".."))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

import validate_calibration as vcal  # noqa: E402

DEFAULT_CM = os.path.join(_REPO, "audio", "overlay", "usr", "local", "bin", "khaen-channelmap")
DEFAULT_PRESETS = os.path.join(_REPO, "audio", "overlay", "etc", "khaen-audio", "layout-presets.yml")
FRAGMENT_META = ("schema", "note", "channelmap_roles")


def _utf8_stdio():
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            try:
                stream.reconfigure(encoding="utf-8", errors="replace")
            except Exception:
                pass


def die(code, msg):
    sys.stderr.write(msg.rstrip() + "\n")
    return code


def load_yaml(path):
    with open(path, "r", encoding="utf-8") as fh:
        return yaml.safe_load(fh)


def dump_yaml(doc, path):
    parent = os.path.dirname(os.path.abspath(path))
    if parent and not os.path.isdir(parent):
        os.makedirs(parent)
    text = yaml.safe_dump(doc, sort_keys=False, allow_unicode=True, default_flow_style=False, width=120)
    tmp = path + ".tmp.%d" % os.getpid()
    with open(tmp, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)
    return tmp


def promote(tmp, path):
    os.replace(tmp, path)


def discard(tmp):
    if tmp and os.path.isfile(tmp):
        try:
            os.remove(tmp)
        except OSError:
            pass


def snapshot(path, backup_dir, label):
    if not backup_dir or not path or not os.path.isfile(path):
        return None
    os.makedirs(backup_dir, exist_ok=True)
    dest = os.path.join(
        backup_dir,
        "%s.%s.%s" % (os.path.basename(path), label, time.strftime("%Y%m%d-%H%M%S")),
    )
    shutil.copy2(path, dest)
    return dest


def same_path(a, b):
    if not a or not b:
        return False
    return os.path.normcase(os.path.abspath(a)) == os.path.normcase(os.path.abspath(b))


def polarity_inverted(value, what):
    if value in (None, "normal", "1", 1):
        return False
    if value in ("invert", "-1", -1):
        return True
    raise ValueError("%s polarity 必须是 normal|invert（或 1/-1），当前 %r" % (what, value))


def load_role_map(spec):
    """None 表示按通道名 = role id。文件是 {校准名: role id}，缺项仍按同名。"""
    if spec is None or spec == "" or spec == "auto":
        return {}
    if not os.path.isfile(spec):
        raise OSError("layout-role-map 不是文件，也不是 auto：%s" % spec)
    data = load_yaml(spec)
    if isinstance(data, dict) and isinstance(data.get("map"), dict):
        data = data["map"]
    if not isinstance(data, dict):
        raise ValueError("layout-role-map 必须是 name: role_id 的 mapping")
    out = {}
    for key, val in data.items():
        if not isinstance(key, str) or not isinstance(val, str) or not key or not val:
            raise ValueError("layout-role-map 的键值必须是非空字符串")
        out[key] = val
    return out


def peq_from_filter(item):
    typ = item.get("type") or "peaking"
    out = {"freq": float(item["freq"]), "q": float(item["q"]), "gain_db": float(item["gain_db"])}
    if typ != "peaking":
        out["type"] = typ
    return out


def apply_channelmap(profile, calibration, role_map):
    """返回 (new_profile, change_lines)。失败抛 ValueError，不改入参。"""
    if not isinstance(profile, dict) or not isinstance(calibration, dict):
        raise ValueError("profile 与 calibration 顶层都必须是 mapping")
    roles = ((profile.get("output") or {}).get("roles"))
    if not isinstance(roles, list):
        raise ValueError("profile.output.roles 必须是数组")
    by_id = {}
    for role in roles:
        if isinstance(role, dict) and isinstance(role.get("id"), str):
            by_id[role["id"]] = role
    channels = calibration.get("channels")
    if not isinstance(channels, list) or not channels:
        raise ValueError("calibration.channels 必须是非空数组")
    fs = calibration.get("sample_rate")
    if not isinstance(fs, int) or isinstance(fs, bool) or fs <= 0:
        raise ValueError("sample_rate 必须是正整数")

    doc = copy.deepcopy(profile)
    new_roles = ((doc.get("output") or {}).get("roles")) or []
    new_by_id = {}
    for role in new_roles:
        if isinstance(role, dict) and isinstance(role.get("id"), str):
            new_by_id[role["id"]] = role

    claimed = {}
    changes = []
    errors = []
    for i, ch in enumerate(channels):
        p = "channels[%d]" % i
        if not isinstance(ch, dict):
            errors.append(p + " 必须是 mapping")
            continue
        name = ch.get("name")
        if not isinstance(name, str) or not name:
            errors.append(p + ".name 缺失")
            continue
        role_id = role_map.get(name, name)
        if role_id in claimed:
            errors.append("%s.name=%s 与 %s 映射到同一个 role %s" % (p, name, claimed[role_id], role_id))
            continue
        claimed[role_id] = name
        role = new_by_id.get(role_id)
        if role is None:
            errors.append("%s.name=%s 在 profile roles 中不存在（映射到 %s）" % (p, name, role_id))
            continue
        try:
            cal_inv = polarity_inverted(ch.get("polarity") or "normal", p)
            role_inv = polarity_inverted(role.get("polarity"), "role %s" % role_id)
        except ValueError as exc:
            errors.append(str(exc))
            continue
        delay = ch.get("delay_samples")
        if not isinstance(delay, int) or isinstance(delay, bool) or delay < 0:
            errors.append(p + ".delay_samples 必须是非负整数")
            continue
        old_delay = role.get("delay_ms")
        if old_delay is None:
            old_delay = 0.0
        if not vcal.is_num(old_delay):
            errors.append("role %s.delay_ms 不是数字" % role_id)
            continue
        new_delay = float(old_delay) + float(delay) * 1000.0 / float(fs)
        if new_delay < -1e-9 or new_delay > 200.0 + 1e-6:
            errors.append("role %s.delay_ms=%s 超出 [0, 200]（拒绝截断）" % (role_id, new_delay))
            continue
        if abs(new_delay) < 1e-9:
            new_delay = 0.0
        elif abs(new_delay - 200.0) < 1e-6:
            new_delay = 200.0
        cal_gain = ch.get("gain_db")
        if cal_gain is None:
            cal_gain = 0.0
        if not vcal.is_num(cal_gain):
            errors.append(p + ".gain_db 不是数字")
            continue
        old_gain = role.get("gain_db")
        if old_gain is None:
            old_gain = 0.0
        if not vcal.is_num(old_gain):
            errors.append("role %s.gain_db 不是数字" % role_id)
            continue
        new_gain = float(old_gain) + float(cal_gain)
        if new_gain < -60.0 - 1e-6 or new_gain > 12.0 + 1e-6:
            errors.append("role %s.gain_db=%s 超出 [-60, +12]（拒绝截断）" % (role_id, new_gain))
            continue
        filters = ch.get("filters")
        if filters is None:
            filters = []
        if not isinstance(filters, list):
            errors.append(p + ".filters 必须是数组")
            continue
        old_peq = role.get("peq")
        if old_peq is None:
            old_peq = []
        if not isinstance(old_peq, list):
            errors.append("role %s.peq 必须是数组" % role_id)
            continue
        if len(old_peq) + len(filters) > 32:
            errors.append("role %s.peq 追加后 %d 段，超过 32" % (role_id, len(old_peq) + len(filters)))
            continue
        try:
            added = [peq_from_filter(item) for item in filters]
        except (KeyError, TypeError, ValueError) as exc:
            errors.append(p + ".filters 无法转成 peq：%s" % exc)
            continue
        new_pol = "invert" if (cal_inv != role_inv) else "normal"
        role["delay_ms"] = new_delay
        role["polarity"] = new_pol
        role["gain_db"] = new_gain
        role["peq"] = list(old_peq) + added
        changes.append(
            "%s <- %s: delay_ms %s -> %s; polarity %s -> %s; gain_db %s -> %s; peq %d -> %d"
            % (
                role_id, name, old_delay, new_delay,
                "invert" if role_inv else "normal", new_pol,
                old_gain, new_gain, len(old_peq), len(role["peq"]),
            )
        )
    if errors:
        raise ValueError("\n".join(errors))
    return doc, changes


def resolve_channelmap(explicit):
    if explicit:
        if os.path.isfile(explicit):
            return explicit
        raise OSError("找不到 khaen-channelmap：%s" % explicit)
    env = os.environ.get("KHAEN_CHANNELMAP")
    if env and os.path.isfile(env):
        return env
    if os.path.isfile(DEFAULT_CM):
        return DEFAULT_CM
    found = shutil.which("khaen-channelmap")
    if found:
        return found
    raise OSError("找不到 khaen-channelmap（可用 --channelmap-bin 或 KHAEN_CHANNELMAP）")


def channelmap_validate(bin_path, profile_path, presets, sample_rate):
    cmd = [sys.executable, bin_path, "validate", profile_path, "--samplerate", str(int(sample_rate))]
    if presets:
        cmd.extend(["--presets", presets])
    env = dict(os.environ)
    env["PYTHONIOENCODING"] = "utf-8"
    env["PYTHONUTF8"] = "1"
    work = tempfile.mkdtemp(prefix="khaen-apply-cm-")
    env["KHAEN_CHANNELMAP_RUN_DIR"] = os.path.join(work, "run")
    env["KHAEN_CHANNELMAP_VAR_DIR"] = os.path.join(work, "var")
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env, cwd=work,
        )
    finally:
        shutil.rmtree(work, ignore_errors=True)
    text = ((proc.stdout or "") + (proc.stderr or "")).strip()
    return proc.returncode, text


def run_channelmap_mode(args):
    if not args.calibration:
        return die(2, "channelmap 模式需要 --calibration")
    if args.fragment:
        return die(2, "不要同时给 --fragment；两种模式分开跑")
    for label, path in (("channelmap", args.channelmap), ("calibration", args.calibration)):
        if not os.path.isfile(path):
            return die(2, "读不到 %s：%s" % (label, path))
    if not args.dry_run and not args.out:
        return die(2, "需要 --out（--dry-run 可以不给）")
    if same_path(args.out, args.channelmap) or same_path(args.out, args.calibration):
        return die(2, "--out 不能与输入文件相同")
    try:
        profile = load_yaml(args.channelmap)
        calibration = load_yaml(args.calibration)
        role_map = load_role_map(args.layout_role_map)
    except Exception as exc:  # noqa: BLE001
        return die(2, "读取失败：%s" % exc)
    report = vcal.check_doc(calibration, args.presets if args.presets and os.path.isfile(args.presets) else None)
    for warning in report.warnings:
        sys.stderr.write("WARN  calibration: %s\n" % warning)
    if report.errors:
        sys.stderr.write("calibration 未通过校验：\n")
        for err in report.errors:
            sys.stderr.write("  ERROR  %s\n" % err)
        return 1
    try:
        merged, changes = apply_channelmap(profile, calibration, role_map)
    except ValueError as exc:
        sys.stderr.write("应用失败：\n")
        for line in str(exc).splitlines():
            sys.stderr.write("  ERROR  %s\n" % line)
        return 1
    sys.stdout.write("channelmap apply（%d 个角色）\n" % len(changes))
    for line in changes:
        sys.stdout.write("  %s\n" % line)
    if args.dry_run:
        sys.stdout.write("dry-run：未写盘\n")
        return 0
    tmp = None
    try:
        tmp = dump_yaml(merged, args.out)
        if not args.no_validate:
            try:
                cm = resolve_channelmap(args.channelmap_bin)
            except OSError as exc:
                discard(tmp)
                return die(2, str(exc))
            rc, text = channelmap_validate(cm, tmp, args.presets, calibration.get("sample_rate") or 48000)
            if rc != 0:
                discard(tmp)
                sys.stderr.write("khaen-channelmap validate 失败（rc=%s）：\n%s\n" % (rc, text))
                return 1
            if text:
                sys.stderr.write(text + "\n")
        src_bak = snapshot(args.channelmap, args.backup_dir, "source")
        prev_bak = snapshot(args.out, args.backup_dir, "previous")
        promote(tmp, args.out)
        tmp = None
    except Exception as exc:  # noqa: BLE001
        discard(tmp)
        return die(2, "写出失败：%s" % exc)
    if args.backup_dir:
        if src_bak:
            sys.stderr.write("backup source -> %s\n" % src_bak)
        if prev_bak:
            sys.stderr.write("backup previous -> %s\n" % prev_bak)
    sys.stdout.write("wrote %s\n" % os.path.abspath(args.out))
    return 0


def resolve_camilla_bin(explicit):
    """返回 (path or None, error or None)。explicit 缺失是用法错误；PATH 缺失则 path=None。"""
    if explicit:
        if os.path.isfile(explicit):
            return explicit, None
        return None, "找不到 camilladsp：%s" % explicit
    return shutil.which("camilladsp"), None


def merge_fragment(base, fragment):
    if not isinstance(base, dict) or not isinstance(fragment, dict):
        raise ValueError("base 与 fragment 顶层都必须是 mapping")
    if "devices" not in base:
        raise ValueError("base 不是完整 CamillaDSP 配置（缺 devices）；fragment 不能直接当配置")
    doc = copy.deepcopy(base)
    base_filters = doc.get("filters") or {}
    frag_filters = fragment.get("filters") or {}
    if not isinstance(base_filters, dict) or not isinstance(frag_filters, dict):
        raise ValueError("filters 必须是 mapping")
    collisions = sorted(set(base_filters) & set(frag_filters))
    if collisions:
        raise ValueError("filter 重名：%s" % ", ".join(collisions))
    merged_filters = dict(base_filters)
    merged_filters.update(copy.deepcopy(frag_filters))
    doc["filters"] = merged_filters
    steps = fragment.get("pipeline") or []
    if not isinstance(steps, list):
        raise ValueError("fragment.pipeline 必须是数组")
    pipe = doc.get("pipeline") or []
    if not isinstance(pipe, list):
        raise ValueError("base.pipeline 必须是数组")
    appended = []
    for i, step in enumerate(steps):
        if not isinstance(step, dict) or step.get("type") != "Filter":
            raise ValueError("fragment.pipeline[%d] 只能是 type: Filter" % i)
        names = step.get("names")
        if not isinstance(names, list) or not names or not all(isinstance(n, str) and n for n in names):
            raise ValueError("fragment.pipeline[%d].names 必须是非空字符串数组" % i)
        missing = [n for n in names if n not in merged_filters]
        if missing:
            raise ValueError("fragment.pipeline[%d] 引用了不存在的 filter：%s" % (i, ", ".join(missing)))
        appended.append(copy.deepcopy(step))
    doc["pipeline"] = list(pipe) + appended
    ignored = [key for key in FRAGMENT_META if key in fragment]
    extra = [key for key in fragment if key not in ("filters", "pipeline") + FRAGMENT_META]
    return doc, sorted(frag_filters), appended, ignored, extra


def camilla_check(bin_path, cfg_path):
    try:
        proc = subprocess.run(
            [bin_path, "--check", cfg_path],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60,
        )
    except Exception as exc:  # noqa: BLE001
        return 1, "camilladsp --check 执行失败：%s" % exc
    text = ((proc.stdout or "") + (proc.stderr or "")).strip()
    return proc.returncode, text


def run_camilladsp_mode(args):
    if not args.fragment:
        return die(2, "camilladsp 模式需要 --fragment")
    if args.channelmap or args.calibration:
        return die(2, "不要同时给 --channelmap/--calibration；两种模式分开跑")
    for label, path in (("camilladsp", args.camilladsp), ("fragment", args.fragment)):
        if not os.path.isfile(path):
            return die(2, "读不到 %s：%s" % (label, path))
    if not args.dry_run and not args.out:
        return die(2, "需要 --out（--dry-run 可以不给）")
    if same_path(args.out, args.camilladsp) or same_path(args.out, args.fragment):
        return die(2, "--out 不能与输入文件相同")
    try:
        base = load_yaml(args.camilladsp)
        fragment = load_yaml(args.fragment)
        merged, added, steps, ignored, extra = merge_fragment(base, fragment)
    except Exception as exc:  # noqa: BLE001
        msg = str(exc)
        code = 2 if msg.startswith("读") else 1
        if isinstance(exc, (OSError, yaml.YAMLError)):
            code = 2
        sys.stderr.write(("读取失败：%s\n" if code == 2 and not msg.startswith("filter") else "合并失败：%s\n") % msg)
        return code
    sys.stdout.write("camilladsp merge：+%d filters，+%d pipeline steps\n" % (len(added), len(steps)))
    for name in added:
        sys.stdout.write("  + filter %s\n" % name)
    for step in steps:
        channels = step.get("channels")
        sys.stdout.write("  + pipeline Filter channels=%s names=%s\n" % (channels, ",".join(step["names"])))
    if ignored:
        sys.stderr.write("WARN  未并入完整配置的片段字段：%s\n" % ", ".join(ignored))
    if extra:
        sys.stderr.write("WARN  fragment 还有未识别字段（已忽略）：%s\n" % ", ".join(extra))
    if args.dry_run:
        sys.stdout.write("dry-run：未写盘\n")
        return 0
    bin_path, err = resolve_camilla_bin(args.camilladsp_bin)
    if err:
        return die(2, err)
    tmp = None
    try:
        tmp = dump_yaml(merged, args.out)
        if bin_path is None:
            sys.stderr.write("WARN  未找到 camilladsp，跳过 --check\n")
        else:
            rc, text = camilla_check(bin_path, tmp)
            if rc != 0:
                discard(tmp)
                sys.stderr.write("camilladsp --check 失败（rc=%s），未写出 %s\n%s\n" % (rc, args.out, text))
                return 1
            if text:
                sys.stderr.write(text + "\n")
        src_bak = snapshot(args.camilladsp, args.backup_dir, "source")
        prev_bak = snapshot(args.out, args.backup_dir, "previous")
        promote(tmp, args.out)
        tmp = None
    except Exception as exc:  # noqa: BLE001
        discard(tmp)
        return die(2, "写出失败：%s" % exc)
    if args.backup_dir:
        if src_bak:
            sys.stderr.write("backup source -> %s\n" % src_bak)
        if prev_bak:
            sys.stderr.write("backup previous -> %s\n" % prev_bak)
    sys.stdout.write("wrote %s\n" % os.path.abspath(args.out))
    return 0


def main(argv=None):
    _utf8_stdio()
    ap = argparse.ArgumentParser(
        description="把校准应用到 channelmap profile，或把 CamillaDSP 片段合并进完整配置。",
    )
    ap.add_argument("--channelmap", help="模式 1：channelmap v2 profile")
    ap.add_argument("--calibration", help="模式 1：khaen-calibration/v1 或 v1.1 YAML")
    ap.add_argument("--layout-role-map", default="auto",
                    help="auto=通道名对 role id；或一个 {name: role_id} YAML")
    ap.add_argument("--channelmap-bin", default=None, help="khaen-channelmap 路径（默认同仓库 overlay）")
    ap.add_argument("--presets", default=DEFAULT_PRESETS if os.path.isfile(DEFAULT_PRESETS) else None,
                    help="layout-presets.yml，传给校验器与 khaen-channelmap")
    ap.add_argument("--no-validate", action="store_true", help="模式 1 不跑 khaen-channelmap validate")
    ap.add_argument("--camilladsp", help="模式 2：完整 CamillaDSP 配置（必须含 devices）")
    ap.add_argument("--fragment", help="模式 2：producer 的 .camilladsp.yml 片段")
    ap.add_argument("--camilladsp-bin", default=None,
                    help="camilladsp 可执行文件；省略则用 PATH，找不到就 WARN 并跳过 --check")
    ap.add_argument("--out", help="输出路径（不覆盖输入；--dry-run 可不给）")
    ap.add_argument("--backup-dir", help="写出前把输入（以及已存在的 --out）复制到这个目录")
    ap.add_argument("--dry-run", action="store_true", help="只打印差异，不写盘")
    args = ap.parse_args(argv)
    if bool(args.channelmap) == bool(args.camilladsp):
        return die(2, "二选一：--channelmap 或 --camilladsp")
    if args.channelmap:
        return run_channelmap_mode(args)
    return run_camilladsp_mode(args)


if __name__ == "__main__":
    sys.exit(main())
