#!/usr/bin/env python3
"""check_resilience.py — 离线校验板级韧性配置（watchdog / panic-on-lockup / pstore / thermal）。"""
import os
import re
import sys

DEPLOY = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
fails, oks = [], []


def ok(m): oks.append(m)
def bad(m): fails.append(m)


def read(p):
    with open(os.path.join(DEPLOY, p), encoding="utf-8") as f:
        return f.read()


def need(text, needle, label):
    ok(label) if needle in text else bad(label + " 缺失('%s')" % needle)


# sysctl
try:
    s = read("config/99-tspi-resilience.conf")
    for k, v in [("kernel.panic_on_oops", "1"), ("kernel.panic", "10"),
                 ("kernel.hardlockup_panic", "1"), ("kernel.softlockup_panic", "1"),
                 ("kernel.hung_task_timeout_secs", "120"), ("kernel.sysrq", "1")]:
        if re.search(r"^\s*%s\s*=\s*%s\s*$" % (re.escape(k), v), s, re.M):
            ok("sysctl %s=%s" % (k, v))
        else:
            bad("sysctl 缺 %s=%s" % (k, v))
    if re.search(r"^\s*kernel\.sched_rt_runtime_us", s, re.M):
        bad("resilience.conf 不应改 sched_rt_runtime_us（保持内核默认 950000，否则 RT 可饿死内核）")
    else:
        ok("未改动 sched_rt_runtime_us（保持 RT 限流）")
except FileNotFoundError:
    bad("config/99-tspi-resilience.conf 缺失")

# watchdog
try:
    w = read("config/10-tspi-watchdog.conf")
    need(w, "RuntimeWatchdogSec=", "systemd RuntimeWatchdogSec")
    need(w, "RebootWatchdogSec=", "systemd RebootWatchdogSec")
    m = re.search(r"RuntimeWatchdogSec=(\d+)s", w)
    if m and 5 <= int(m.group(1)) <= 120:
        ok("watchdog 周期合理(%ss)" % m.group(1))
    elif m:
        bad("watchdog 周期 %ss 不合理（应 5–120s）" % m.group(1))
except FileNotFoundError:
    bad("config/10-tspi-watchdog.conf 缺失")

# pstore
try:
    p = read("config/pstore-archive")
    for needle, label in [("/sys/fs/pstore", "读 /sys/fs/pstore"),
                          ("/var/log/tspi/pstore", "归档到 /var/log/tspi/pstore"),
                          ("INDEX.txt", "空 pstore 也留 INDEX 记录"),
                          ("boot_id", "记录 boot_id")]:
        need(p, needle, label)
except FileNotFoundError:
    bad("config/pstore-archive 缺失")
try:
    u = read("config/tspi-pstore-archive.service")
    need(u, "ExecStart=/usr/local/lib/tspi/pstore-archive", "pstore unit ExecStart 路径")
    need(u, "Before=", "pstore unit 有排序约束")
except FileNotFoundError:
    bad("config/tspi-pstore-archive.service 缺失")

# thermal
try:
    t = read("config/thermal-log")
    need(t, "thermal.log", "thermal 日志路径")
    need(t, "thermal_zone", "采集 thermal_zone")
    need(t, "sched_rt_runtime_us", "记录 RT 限流值")
except FileNotFoundError:
    bad("config/thermal-log 缺失")
try:
    tm = read("config/tspi-thermal-log.timer")
    m = re.search(r"^OnUnitActiveSec=(\d+)s", tm, re.M)
    if m and int(m.group(1)) <= 300:
        ok("thermal timer 周期 %ss" % m.group(1))
    else:
        bad("thermal timer 周期缺失或 >300s")
except FileNotFoundError:
    bad("config/tspi-thermal-log.timer 缺失")

# installer wiring
try:
    i = read("bin/04-install-release.sh")
    for needle, label in [("99-tspi-resilience.conf", "安装 sysctl"),
                          ("10-tspi-watchdog.conf", "安装 watchdog 配置"),
                          ("pstore-archive", "安装 pstore 归档"),
                          ("thermal-log", "安装 thermal 日志")]:
        need(i, needle, "04-install " + label)
except FileNotFoundError:
    bad("bin/04-install-release.sh 缺失")

# verify wiring
try:
    v = read("bin/06-verify-switch.sh")
    for needle in ["V11.watchdog", "V11.panic", "V11.pstore", "V11.thermal"]:
        need(v, needle, "06-verify " + needle)
except FileNotFoundError:
    bad("bin/06-verify-switch.sh 缺失")

print("PASS %d  FAIL %d" % (len(oks), len(fails)))
for m in fails:
    print("  [FAIL]", m)
sys.exit(1 if fails else 0)
