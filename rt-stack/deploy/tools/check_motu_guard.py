#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""check_motu_guard.py — 离线静态校验 MOTU 48k 防线（不连板、不需要 bash）。

校验项：
  1. keeper 脚本存在、含 stageA/stageB、含 flock、日志路径 /var/log/khaen
  2. keeper 的 WRITE_BACK 默认值为 0（写回未证，不允许默认开）
  3. timer 含 OnUnitActiveSec=60s（<=120s）
  4. 04-install-release.sh 会把 keeper/motu-rate-guard.env/cuemix_client.py 装到位
  5. 06-verify 的 V9 组包含可证伪检查（周期/新鲜度/设备速率）
  6. REBOOT-RECOVERY-CHECKLIST 的 healthz 用 :80 而不是 8090
"""
import os
import re
import sys

DEPLOY = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
fails = []
oks = []


def ok(m):
    oks.append(m)


def bad(m):
    fails.append(m)


def read(p):
    with open(os.path.join(DEPLOY, p), encoding="utf-8") as f:
        return f.read()


# 1 keeper
try:
    k = read("config/motu-48k-keeper")
except FileNotFoundError:
    bad("config/motu-48k-keeper 缺失")
    k = ""
if k:
    for needle, label in [("stageA", "Stage A(ALSA 空闲兜底)"),
                          ("stageB", "Stage B(CueMix 只读速率)"),
                          ("flock", "flock 并发保护"),
                          ("/var/log/khaen", "持久日志目录"),
                          ("aplay", "aplay 48k 静音流")]:
        ok(f"keeper 含 {label}") if needle in k else bad(f"keeper 缺 {label}('{needle}')")
    m = re.search(r"^WRITE_BACK=(\d+)", k, re.M)
    if not m:
        bad("keeper 未定义 WRITE_BACK 默认值")
    elif m.group(1) != "0":
        bad(f"keeper WRITE_BACK 默认 {m.group(1)}，必须为 0（CueMix 写 kSampleRate 未证）")
    else:
        ok("keeper WRITE_BACK 默认 0")
    # 写回必须被开关包住
    if "WRITE_BACK:-0}" in k or "WRITE_BACK:-0" in k:
        ok("keeper 写回受 WRITE_BACK 开关控制")
    else:
        bad("keeper 写回未被 WRITE_BACK 开关包住")
    if "cuemix_client.py" in k and "/usr/local/lib/khaen/cuemix_client.py" in k:
        ok("keeper 会优先用已安装的 cuemix_client.py")
    else:
        bad("keeper 未解析 /usr/local/lib/khaen/cuemix_client.py")

# 2 timer
try:
    t = read("config/khaen-motu-48k-keeper.timer")
    m = re.search(r"^OnUnitActiveSec=(\d+)s", t, re.M)
    if not m:
        bad("timer 缺 OnUnitActiveSec=<n>s")
    else:
        n = int(m.group(1))
        ok(f"timer 周期 {n}s") if n <= 120 else bad(f"timer 周期 {n}s > 120s（192k 漂移窗口靠它兜）")
    ok("timer 有 OnBootSec") if "OnBootSec=" in t else bad("timer 缺 OnBootSec")
except FileNotFoundError:
    bad("config/khaen-motu-48k-keeper.timer 缺失")

# 3 env template
try:
    e = read("config/motu-rate-guard.env")
    ok("motu-rate-guard.env 含 WRITE_BACK=0") if re.search(r"^WRITE_BACK=0", e, re.M) else bad("motu-rate-guard.env 未默认 WRITE_BACK=0")
except FileNotFoundError:
    bad("config/motu-rate-guard.env 缺失")

# 4 installer
try:
    i = read("bin/04-install-release.sh")
    for needle, label in [("motu-rate-guard.env", "安装 motu-rate-guard.env"),
                          ("tools/cuemix_client.py", "安装 cuemix_client.py"),
                          ("config/motu-48k-keeper", "安装 keeper 脚本"),
                          ("khaen-motu-48k-keeper.timer", "安装 timer")]:
        ok(f"04-install 含 {label}") if needle in i else bad(f"04-install 缺 {label}('{needle}')")
    ok("04-install 不覆盖既有 rate-guard.env") if "不覆盖运维已调开关" in i else bad("04-install 会覆盖既有 /etc/khaen/motu-rate-guard.env？")
except FileNotFoundError:
    bad("bin/04-install-release.sh 缺失")

# 5 verify
try:
    v = read("bin/06-verify-switch.sh")
    for needle, label in [("V9.keeper.period", "V9.keeper.period 周期检查"),
                          ("V9.keeper.run", "V9.keeper.run 新鲜度检查"),
                          ("V9.device.rate", "V9.device.rate 设备速率检查"),
                          ("-5 min", "近 5min 新鲜度窗口")]:
        ok(f"06-verify 含 {label}") if needle in v else bad(f"06-verify 缺 {label}('{needle}')")
    if "V9.keeper.run NOT_VERIFIED" in v:
        bad("06-verify 仍把 keeper 未跑记成 NOT_VERIFIED（应 FAIL）")
    else:
        ok("06-verify 不再用 NOT_VERIFIED 放过未跑的 keeper")
except FileNotFoundError:
    bad("bin/06-verify-switch.sh 缺失")

# 6 checklist port
try:
    c = read("REBOOT-RECOVERY-CHECKLIST.md")
    if ":80/healthz" not in c:
        bad("REBOOT-RECOVERY-CHECKLIST 未使用 :80/healthz")
    elif re.search(r"127\.0\.0\.1:8090/healthz", c):
        bad("REBOOT-RECOVERY-CHECKLIST 仍残留 127.0.0.1:8090/healthz")
    else:
        ok("REBOOT-RECOVERY-CHECKLIST 用 :80/healthz")
except FileNotFoundError:
    bad("REBOOT-RECOVERY-CHECKLIST.md 缺失")

print(f"PASS {len(oks)}  FAIL {len(fails)}")
for m in oks:
    print("  [PASS]", m)
for m in fails:
    print("  [FAIL]", m)
sys.exit(1 if fails else 0)
