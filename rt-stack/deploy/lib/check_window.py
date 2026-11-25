#!/usr/bin/env python3
"""T28 授权窗口校验（deploy/bin 的 live 前置门）。

用法: check_window.py <窗口 JSON> <逗号分隔 scope>
退出码: 0 OK；4 窗口缺失/过期/字段不符/scope 不覆盖。
只读：不写文件、不联网。
"""
import json
import sys
import datetime

SCHEMA = "tspi-rt-stack/switch-window/v1"
REQUIRED = (
    "window_id",
    "authorized_by",
    "board_host",
    "start",
    "end",
    "max_duration_min",
    "scope",
    "audio_off_required",
    "s1_stop_acknowledged",
    "soak_absent",
)


def fail(msg):
    print(json.dumps({"ok": False, "error": msg}, ensure_ascii=False))
    sys.exit(4)


def parse_ts(s):
    try:
        d = datetime.datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except ValueError:
        fail(f"时间不可解析: {s!r}（要求 ISO-8601 带时区，如 2026-11-20T10:00:00+08:00）")
    if d.tzinfo is None:
        fail(f"时间缺少时区: {s!r}")
    return d


def main():
    if len(sys.argv) != 3:
        fail("用法: check_window.py <窗口 JSON> <scope1,scope2,...>")
    path, scopes = sys.argv[1], [s for s in sys.argv[2].split(",") if s]
    try:
        with open(path, encoding="utf-8") as f:
            d = json.load(f)
    except OSError as e:
        fail(f"窗口文件不可读: {e}")
    except json.JSONDecodeError as e:
        fail(f"窗口文件不是合法 JSON: {e}")
    if not isinstance(d, dict):
        fail("窗口文件顶层必须是对象")
    if d.get("schema") != SCHEMA:
        fail(f"schema 必须是 {SCHEMA}，实际 {d.get('schema')!r}")
    missing = [k for k in REQUIRED if k not in d]
    if missing:
        fail(f"缺少必填字段: {missing}")
    for flag in ("audio_off_required", "s1_stop_acknowledged", "soak_absent"):
        if d.get(flag) is not True:
            fail(f"{flag} 必须为 true（切换前必须 audio off、已确认停 S1、无 soak 在跑）")
    try:
        max_min = int(d["max_duration_min"])
    except (TypeError, ValueError):
        fail("max_duration_min 必须是整数分钟")
    if max_min <= 0 or max_min > 120:
        fail(f"max_duration_min={max_min} 超出允许范围 1..120")
    start = parse_ts(d["start"])
    end = parse_ts(d["end"])
    if end <= start:
        fail("end 必须晚于 start")
    if (end - start).total_seconds() > max_min * 60:
        fail("窗口时长超过 max_duration_min")
    now = datetime.datetime.now().astimezone()
    if now < start:
        fail(f"授权窗口尚未开始（start={d['start']}，now={now.isoformat()}）")
    if now > end:
        fail(f"授权窗口已过期（end={d['end']}，now={now.isoformat()}）")
    granted = d.get("scope") or []
    if not isinstance(granted, list):
        fail("scope 必须是数组")
    missing_scope = [s for s in scopes if s not in granted]
    if missing_scope:
        fail(f"窗口 scope 未覆盖本次动作: {missing_scope}（窗口只授予 {granted}）")
    print(
        json.dumps(
            {
                "ok": True,
                "window_id": d["window_id"],
                "authorized_by": d["authorized_by"],
                "board_host": d["board_host"],
                "start": d["start"],
                "end": d["end"],
                "scope": granted,
                "checked_at": now.isoformat(),
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
