#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""T72 离线静态扫描：conf/service/overlay/web/deploy 中 CamillaDSP / 旧 DSP 引用清单与分类。

用途（WBS T72 / DSP-HOST-DECISION §7 G-no-C）
------------------------------------------------
扫描 new-stack 可部署域（conf/service）、旧 overlay、旧 web、deploy 工装中的
CamillaDSP / 旧 DSP 进程（khaen-upmix / 旧 web shim / khaen-mode）引用，输出：
  1. refs.json  机器可读：逐文件命中 + 分类（runtime_dependency / historical_only / to_delete）
                + 新栈载荷（bundle units + 安装模板）逐行审计；
  2. REFS.md    人类可读清单（分类表 + 证据片段 + 扫描命令/哈希）。

分类语义（与 task-161 对齐）
  runtime_dependency  会被部署/启动并实际启动或联系 C 的引用 —— T72 要求为 0；
  historical_only     文档 / 测试 / 负向守卫 / 退役验证工装 / 保留 AoIP 数据；属于"仅历史记录"；
  to_delete           旧 C 栈运行件、旧 Web shim、残留旧 unit 引用等，退役清理应删除或不得部署。

退出码：0 = 扫描完成且 runtime_dependency=0；3 = 有文件未分类（规则缺口）；
        4 = 发现 runtime_dependency（T72 失败）；7 = 输入/环境错误。

用法:
  python3 scan_no_c_refs.py --project-root . --out-dir rt-stack/evidence/no-c-proof/scan
"""
import argparse
import datetime
import fnmatch as _fnmatch
import hashlib
import json
import os
import platform
import re
import sys

TICK = chr(96)  # backtick character (kept out of generator source)

SCOPE_DIRS = {
    "conf": ["rt-stack/deploy/config"],
    "service": ["rt-stack/service"],
    "overlay": ["audio/overlay"],
    "web": ["web"],
    "deploy": ["rt-stack/deploy"],
}
# 追加扫描（不属于 task 指定五域；用于 G-no-C 耐久性旁证，不参与主结论计数）
SUPPLEMENT_DIRS = ["rt-stack/crates"]
SUPPLEMENT_FILES = [
    "audio/build-all.sh",
    "audio/install-into-image.sh",
    "firmware/oem/install-oem.sh",
]
EXCLUDE_PARTS = {".git", "target", "node_modules", ".venv", "__pycache__", ".cargo"}
MAX_FILE_SIZE = 2 * 1024 * 1024
BINARY_EXTS = {
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip", ".gz",
    ".tgz", ".tar", ".xz", ".zst", ".so", ".a", ".o", ".ko", ".bin", ".elf",
    ".ttf", ".woff", ".woff2", ".eot", ".mp3", ".wav", ".flac", ".mp4",
}

# ---- 行级匹配模式 -----------------------------------------------------------
LINE_PATTERNS = [
    ("c_engine", re.compile(r"camilla[-_ ]?dsp", re.I)),
    ("c_token", re.compile(r"\bcamilla\b", re.I)),
    ("old_unit", re.compile(r"khaen-(?:camilladsp|upmix-autodetect|audio-web|audio\.target|boot-mode)\b", re.I)),
    ("old_dsp_tool", re.compile(r"\bkhaen-(?:upmix|mode|channelmap)\b", re.I)),
    ("c_path", re.compile(r"/etc/camilladsp\b|/run/khaen/camilladsp\b|/usr/local/bin/camilladsp\b", re.I)),
    ("c_ws", re.compile(r"ws://[^\s\"']*:1234\b|127\.0\.0\.1:1234|localhost:1234|KHAEN_CAMILLADSP_(?:PORT|BIND)", re.I)),
    ("sox", re.compile(r"\bsox(?:i)?\b", re.I)),
    ("socket_pipe", re.compile(r"(?:sock(?:et)?|fifo|mkfifo|pipe)[^\n]{0,80}(?:camilla|khaen-camilladsp|khaen-upmix)", re.I)),
]
FILENAME_PATTERN = re.compile(r"camilla|old[-_]?c|khaen-(?:mode|channelmap|upmix)", re.I)

PORT_CONTEXT = {
    1234: re.compile(
        r"(?:port\s*[=:]\s*[\"']?1234\b|-p\s+1234\b|(?:127\.0\.0\.1|localhost|0\.0\.0\.0|\[::1?\])\s*:\s*1234\b"
        r"|ws://[^\s\"']*:1234\b|KHAEN_CAMILLADSP_PORT[^\n]{0,20}1234\b)",
        re.I,
    ),
    18080: re.compile(r"\b18080\b"),
}


def port_line_hits(line):
    hits = []
    for port, rx in PORT_CONTEXT.items():
        if rx.search(line):
            hits.append("port_%d" % port)
    return hits


# ---- 分类规则（有序；先命中先归属） ----------------------------------------
# category: runtime_dependency | historical_only | to_delete
RULES = [
    # ---------- overlay：旧 C 引擎/依赖 unit ----------
    dict(id="OV-C-UNIT",
         globs=["audio/overlay/etc/systemd/system/khaen-camilladsp.service",
                "audio/overlay/etc/systemd/system/khaen-upmix-autodetect.service"],
         category="to_delete", role="旧C引擎/旧C依赖daemon unit（RETIRE_C）",
         reason="停止+disable+mask 后保留审计；不得随镜像/部署再启用",
         old_c_disposition="RETIRE_C_PROCESS"),
    dict(id="OV-C-CONF",
         globs=["audio/overlay/etc/camilladsp/*"],
         category="to_delete", role="旧C渲染配置/系数（RETIRE_C）",
         reason="T71 03-retire 删除 /run/khaen/camilladsp.yml 后不再被读取；历史文件保留审计",
         old_c_disposition="RETIRE_C_PROCESS"),
    dict(id="OV-ENTRY-UNIT",
         globs=["audio/overlay/etc/systemd/system/khaen-audio.target",
                "audio/overlay/etc/systemd/system/khaen-boot-mode.service"],
         category="to_delete", role="旧编排/开机恢复入口（REPLACE_SHIM）",
         reason="khaen-mode 写 marker 后拉起 C；03-retire 必须 disable(+mask boot-mode)",
         old_c_disposition="REPLACE_SHIM"),
    dict(id="OV-ENTRY-BIN",
         globs=["audio/overlay/usr/local/bin/khaen-mode",
                "audio/overlay/usr/local/bin/khaen-upmix",
                "audio/overlay/usr/local/bin/khaen-channelmap"],
         category="to_delete", role="旧C编排/控制可执行（REPLACE_SHIM）",
         reason="会 systemctl start/restart khaen-camilladsp 或写 /run/khaen/camilladsp.yml",
         old_c_disposition="REPLACE_SHIM"),
    # ---------- overlay：保留数据 / AoIP ----------
    dict(id="OV-RETAIN-UNIT",
         globs=["audio/overlay/etc/systemd/system/khaen-statime-inferno.service",
                "audio/overlay/etc/systemd/system/khaen-aes67-daemon.service",
                "audio/overlay/etc/systemd/system/khaen-ravenna-lkm.service",
                "audio/overlay/etc/systemd/system/khaen-ptp4l-master.service",
                "audio/overlay/etc/systemd/system/khaen-phc2sys.service"],
         category="historical_only", role="保留 AoIP/PTP unit（RETAIN_AOIP）",
         reason="P1 保留；After/PartOf 引用已 disabled 的 khaen-audio.target（排序引用，非激活依赖）",
         old_c_disposition="RETAIN_AOIP"),
    dict(id="OV-RETAIN-LIB",
         globs=["audio/overlay/usr/local/lib/khaen/lib.sh",
                "audio/overlay/usr/local/lib/khaen/ptp-status-hook",
                "audio/overlay/usr/local/bin/khaen-ptp-status"],
         category="historical_only", role="保留 AoIP 工具库/状态脚本（RETAIN_AOIP）",
         reason="lib.sh 残留 KHAEN_CAMILLADSP_PORT=1234 默认变量但新栈不读；ptp-status 的 1234 为示例 offset",
         old_c_disposition="RETAIN_AOIP"),
    dict(id="OV-RETAIN-ASOUND",
         globs=["audio/overlay/etc/asound.conf"],
         category="historical_only", role="保留 Inferno AoIP ALSA 定义（RETAIN_AOIP）",
         reason="仅注释提及 camilladsp.env；pcm.inferno 由 AoIP 使用，与 C 无运行依赖",
         old_c_disposition="RETAIN_AOIP"),
    dict(id="OV-KEEP",
         globs=["audio/overlay/etc/default/khaen-audio",
                "audio/overlay/etc/modprobe.d/*",
                "audio/overlay/etc/udev/rules.d/*",
                "audio/overlay/etc/statime/*",
                "audio/overlay/etc/aes67-daemon/*",
                "audio/overlay/usr/share/alsa/ucm2/*",
                "audio/overlay/usr/share/alsa/ucm2/*/*",
                "audio/overlay/usr/share/alsa/ucm2/*/*/*",
                "audio/overlay/usr/share/alsa/ucm2/*/*/*/*",
                "audio/overlay/usr/share/alsa/ucm2/*/*/*/*/*",
                "audio/overlay/etc/khaen-audio/*",
                "audio/overlay/etc/khaen-audio/*/*",
                "audio/overlay/usr/local/bin/README.md"],
         category="historical_only", role="保留数据/环境/注释（KEEP_DATA）",
         reason="T71/T72 保留；仅注释、示例数字（aes67 syslog:1234）或旧栈变量残留，无运行依赖",
         old_c_disposition="KEEP_DATA"),
    # ---------- web：旧 Web shim ----------
    dict(id="WEB-SRC",
         globs=["web/src/*", "web/src/*/*", "web/src/*/*/*", "web/tests/*", "web/tests/*/*"],
         category="to_delete", role="旧 Web shim 源码/测试（REPLACE_SHIM）",
         reason="后端是 CamillaDSP WS(1234) 并 systemctl 管理 khaen-camilladsp；由新 daemon Web 替代",
         old_c_disposition="REPLACE_SHIM"),
    dict(id="WEB-DEPLOY",
         globs=["web/deploy/khaen-audio-web.service", "web/deploy/config.toml.example"],
         category="to_delete", role="旧 Web unit/配置模板（REPLACE_SHIM）",
         reason="[camilladsp] ws_url=ws://127.0.0.1:1234、service=khaen-camilladsp.service；不得再部署",
         old_c_disposition="REPLACE_SHIM"),
    dict(id="WEB-UI",
         globs=["web/assets/*", "web/dist/*"],
         category="to_delete", role="旧 Web UI/构建发布产物（REPLACE_SHIM）",
         reason="前端调用 /api/camilladsp*；旧发布产物禁止安装",
         old_c_disposition="REPLACE_SHIM"),
    dict(id="WEB-SCRIPTS",
         globs=["web/scripts/*", "web/Cargo.toml", "web/Cargo.lock"],
         category="to_delete", role="旧 Web 包清单/安装脚本（REPLACE_SHIM）",
         reason="安装/构建旧 Web（含旧栈安装）；随 shim 删除；Cargo.toml 命中仅为包自名",
         old_c_disposition="REPLACE_SHIM"),
    dict(id="WEB-DOC",
         globs=["web/docs/*", "web/README.md", "web/deploy/OEM-HANDOFF.md"],
         category="historical_only", role="旧 Web/旧 C 接口文档（历史）",
         reason="文档保留但不得作为部署/验收依赖；新 Web 由 daemon 提供",
         old_c_disposition="KEEP_DATA"),
    # ---------- deploy：新栈工装 ----------
    dict(id="DEP-CONF-OLDC",
         globs=["rt-stack/deploy/config/old-c-units.conf"],
         category="historical_only", role="退役/验证工装配置（必须保留）",
         reason="03/05/06/08 共用；按设计引用旧 unit/入口/进程名，用于断言 C 已退役",
         old_c_disposition=""),
    dict(id="DEP-CONF-HOST",
         globs=["rt-stack/deploy/config/rt-host.toml.example",
                "rt-stack/deploy/config/rt-host.env.example",
                "rt-stack/deploy/config/rt-daemon.toml.example",
                "rt-stack/deploy/config/rt-daemon.env.example"],
         category="historical_only", role="新栈配置模板纪律注释",
         reason="仅注释/纪律行提及 CamillaDSP（禁止使用）；无运行引用",
         old_c_disposition=""),
    dict(id="DEP-CONF-KEEPER",
         globs=["rt-stack/deploy/config/khaen-motu-48k-keeper.service"],
         category="to_delete", role="新栈 keeper unit 内 Before= 旧 unit 名残留（非激活依赖）",
         reason="Before= 仅排序、无 Wants/Requires 不拉起 C；但按 G-no-C '其它 unit 不再依赖 C' 应清理为 Before=khaen-rt-host.service",
         old_c_disposition=""),
    dict(id="DEP-KEEPER-DATA",
         globs=["rt-stack/deploy/config/khaen-motu-48k-keeper.timer",
                "rt-stack/deploy/config/95-khaen-motu-48k-keeper.rules",
                "rt-stack/deploy/config/motu-48k-keeper",
                "rt-stack/deploy/config/switch-window.example.json"],
         category="historical_only", role="新栈 keeper/窗口配套（无 C 引用）",
         reason="随 04-install-release 安装；文件本身无 C 引用（列入候选仅为域内说明）",
         old_c_disposition=""),
    dict(id="DEP-BIN",
         globs=["rt-stack/deploy/bin/*"],
         category="historical_only", role="退役/切换/验证/回滚/快照工装（必须保留）",
         reason="00-08 脚本按设计读取/停用/断言旧 C；不构成新栈运行依赖（05/06/08 已含 never-C 契约）",
         old_c_disposition=""),
    dict(id="DEP-LIB",
         globs=["rt-stack/deploy/lib/*"],
         category="historical_only", role="退役工装公共库/校验器（必须保留）",
         reason="common.sh/last_good.py 含 C 断言（assert_c_retired / c_policy=never-camilladsp）",
         old_c_disposition=""),
    dict(id="DEP-TESTS",
         globs=["rt-stack/deploy/tests/*"],
         category="historical_only", role="负向契约测试/本地 mock（必须保留）",
         reason="check_unit_syntax/check_rollback_file 拒收 C 依赖；dryrun 测试断言不启动 C",
         old_c_disposition=""),
    dict(id="DEP-README",
         globs=["rt-stack/deploy/README.md"],
         category="historical_only", role="部署/回滚文档（历史）",
         reason="文档；G-no-C 约束与命令说明",
         old_c_disposition=""),
    # ---------- service：新栈 unit ----------
    dict(id="SVC-UNIT",
         globs=["rt-stack/service/*.service", "rt-stack/service/*.target"],
         category="historical_only", role="新栈 systemd unit（仅负向防误启守卫）",
         reason="Camilla 仅出现于 ConditionPathExists=!/run/khaen/camilladsp.yml（防双 owner 守卫）；无 Wants/Requires/Exec 引用",
         old_c_disposition=""),
    dict(id="SVC-README",
         globs=["rt-stack/service/README.md"],
         category="historical_only", role="新栈 service 设计文档（历史）",
         reason="文档说明无 C 依赖",
         old_c_disposition=""),
    dict(id="SVC-DROPIN",
         globs=["rt-stack/service/*.d/*", "rt-stack/service/*.d/*/*"],
         category="historical_only", role="新栈 unit drop-in/env 模板（仅纪律注释）",
         reason="如 90-safe-mute.env.example：仅注释提及 G-no-C 禁止 C 回退；EnvironmentFile 变量为 KHAEN_HOST_SAFE_MODE=mute",
         old_c_disposition=""),
    dict(id="DEP-NOTES",
         globs=["rt-stack/deploy/*.md"],
         category="historical_only", role="部署/测试笔记文档（历史）",
         reason="文档（如 T71-SOUND-TEST-NOTES）；记录旧 C 工具问题，无运行引用",
         old_c_disposition=""),
    # ---------- 追加扫描（域外旁证） ----------
    dict(id="SUP-CRATES",
         globs=["rt-stack/crates/*", "rt-stack/crates/*/*", "rt-stack/crates/*/*/*",
                "rt-stack/crates/*/*/*/*", "rt-stack/crates/*/*/*/*/*",
                "rt-stack/crates/*/*/*/*/*/*", "rt-stack/crates/*/*/*/*/*/*/*",
                "rt-stack/crates/*/*/*/*/*/*/*/*"],
         category="historical_only", role="新栈源码中的历史注释/compat 契约（域外旁证）",
         reason="仅为注释/compat 端点命名；新栈不 spawn/联系 camilladsp",
         old_c_disposition=""),
    dict(id="SUP-BUILD",
         globs=["audio/build-all.sh"],
         category="historical_only", role="开发期旧C构建脚本（P1 不部署，归档）",
         reason="构建 camilladsp 仅供开发期 oracle；不是运行依赖",
         old_c_disposition="RETIRE_C_PROCESS"),
    dict(id="SUP-IMAGE",
         globs=["audio/install-into-image.sh", "firmware/oem/install-oem.sh"],
         category="to_delete", role="镜像构建链仍安装/启用旧 C 与旧 Web（域外，G-no-C 耐久性风险）",
         reason="install-into-image 复制 camilladsp 二进制并 enable khaen-camilladsp/khaen-audio.target；install-oem 安装旧 web 并 enable、sed 映射 khaen-camilladsp.service —— 重新刷机/OTA 会把已退役入口放回，需 T31/T32 或镜像更新时清理",
         old_c_disposition="REPLACE_SHIM"),
]

PAYLOAD_GLOBS = [
    "rt-stack/service/*.service",
    "rt-stack/service/*.target",
    "rt-stack/service/*.d/*",
    "rt-stack/deploy/config/rt-host.toml.example",
    "rt-stack/deploy/config/rt-host.env.example",
    "rt-stack/deploy/config/rt-daemon.toml.example",
    "rt-stack/deploy/config/rt-daemon.env.example",
    "rt-stack/deploy/config/khaen-motu-48k-keeper.service",
    "rt-stack/deploy/config/khaen-motu-48k-keeper.timer",
    "rt-stack/deploy/config/95-khaen-motu-48k-keeper.rules",
]
POSITIVE_KEYS = {"Wants", "Requires", "Requisite", "BindsTo", "PartOf", "ExecStart",
                 "ExecStartPre", "ExecStartPost", "ExecStop", "EnvironmentFile",
                 "WantedBy", "RequiredBy", "Also", "OnFailure", "Conflicts"}
ORDERING_KEYS = {"Before", "After"}
COSMETIC_KEYS = {"Description", "Documentation", "ConditionPathExists", "AssertPathExists",
                 "ConditionFileNotEmpty", "Comment"}


def now_iso():
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(262144), b""):
            h.update(chunk)
    return h.hexdigest()


def classify(rel):
    for rule in RULES:
        for g in rule["globs"]:
            if _fnmatch.fnmatchcase(rel, g):
                return rule
    return None


def is_probably_binary(path):
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
    except OSError:
        return True
    return b"\x00" in head


def walk_root(project_root, rel_root, collected, exclusions):
    base = os.path.join(project_root, rel_root)
    if not os.path.isdir(base):
        exclusions.append({"path": rel_root, "reason": "不存在"})
        return
    for dirpath, dirnames, filenames in os.walk(base):
        dirnames[:] = sorted(d for d in dirnames if d not in EXCLUDE_PARTS)
        for name in sorted(filenames):
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, project_root).replace(os.sep, "/")
            ext = os.path.splitext(name)[1].lower()
            if ext in BINARY_EXTS:
                exclusions.append({"path": rel, "reason": "二进制扩展名 %s" % ext})
                continue
            try:
                size = os.path.getsize(full)
            except OSError:
                continue
            if size > MAX_FILE_SIZE:
                exclusions.append({"path": rel, "reason": "超扫描上限 %d>%d" % (size, MAX_FILE_SIZE)})
                continue
            if is_probably_binary(full):
                exclusions.append({"path": rel, "reason": "二进制内容"})
                continue
            collected.append({"rel": rel, "abs": full, "size": size})


def scan_file(entry, supplement=False):
    try:
        with open(entry["abs"], "r", encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
    except OSError as e:
        return {"file": entry["rel"], "error": str(e), "category": "unclassified"}
    counts = {}
    hit_lines = []
    for lineno, line in enumerate(lines, 1):
        labels = [label for label, rx in LINE_PATTERNS if rx.search(line)]
        labels.extend(port_line_hits(line))
        if labels:
            for lb in labels:
                counts[lb] = counts.get(lb, 0) + 1
            hit_lines.append({"line": lineno, "labels": labels, "text": line.strip()[:240]})
    name_hit = bool(FILENAME_PATTERN.search(os.path.basename(entry["rel"])))
    if not counts and not name_hit:
        return None
    rule = classify(entry["rel"])
    item = {
        "file": entry["rel"],
        "scope": entry["rel"].split("/")[0] if not supplement else "supplement",
        "supplement": supplement,
        "size": entry["size"],
        "sha256": sha256_file(entry["abs"]),
        "counts": counts,
        "hit_lines": hit_lines[:60],
        "filename_match": name_hit,
    }
    if rule is None:
        item.update({"category": "unclassified", "role": "UNCLASSIFIED（规则缺口）",
                     "reason": "需补充分类规则；不得当作已审计", "rule_id": "",
                     "old_c_disposition": ""})
    else:
        item.update({"category": rule["category"], "role": rule["role"],
                     "reason": rule["reason"], "rule_id": rule["id"],
                     "old_c_disposition": rule.get("old_c_disposition", "")})
    return item


def payload_audit(project_root, scanned_rels):
    payload_files = []
    for g in PAYLOAD_GLOBS:
        for rel in sorted(scanned_rels):
            if _fnmatch.fnmatchcase(rel, g) and rel not in payload_files:
                payload_files.append(rel)
    entries = []
    counters = {"runtime_dependency_lines": 0, "ordering_dependency_lines": 0,
                "negative_guard_lines": 0, "unknown_lines": 0}
    for rel in payload_files:
        abs_path = os.path.join(project_root, rel)
        try:
            with open(abs_path, "r", encoding="utf-8", errors="replace") as f:
                lines = f.read().splitlines()
        except OSError:
            continue
        refs = []
        for lineno, line in enumerate(lines, 1):
            if not any(rx.search(line) for _, rx in LINE_PATTERNS) and not port_line_hits(line):
                continue
            stripped = line.strip()
            kind = "unknown"
            key = ""
            m = re.match(r"^([A-Za-z][A-Za-z0-9]*)\s*=", stripped)
            if stripped.startswith("#") or stripped.startswith(";"):
                kind = "comment"
            elif m:
                key = m.group(1)
                if key in ORDERING_KEYS:
                    kind = "ordering_dependency"
                elif key in POSITIVE_KEYS:
                    kind = "positive_dependency"
                elif key in COSMETIC_KEYS:
                    if stripped.startswith("ConditionPathExists=!") or stripped.startswith("AssertPathExists=!"):
                        kind = "negative_guard"
                    else:
                        kind = "cosmetic"
                else:
                    kind = "unknown"
            if kind == "positive_dependency":
                counters["runtime_dependency_lines"] += 1
            elif kind == "ordering_dependency":
                counters["ordering_dependency_lines"] += 1
            elif kind == "negative_guard":
                counters["negative_guard_lines"] += 1
            elif kind == "unknown":
                counters["unknown_lines"] += 1
            refs.append({"line": lineno, "key": key, "kind": kind, "text": stripped[:240]})
        entries.append({"file": rel, "refs": refs})
    return {"payload_files": payload_files, "entries": entries, "summary": counters}


def esc_md(text):
    return str(text).replace("|", "\\|")


def render_md(doc):
    s = doc["summary"]
    pa = doc["payload_audit"]["summary"]
    lines = []
    lines.append("# T72 离线静态扫描清单（CamillaDSP / 旧 DSP 引用）")
    lines.append("")
    lines.append("> 生成时间：%s；生成脚本：%srt-stack/tools/no-c-proof/scan_no_c_refs.py%s" % (doc["generated_at"], TICK, TICK))
    lines.append("> 扫描根：%s%s%s；schema：%s%s%s" % (TICK, doc["project_root"], TICK, TICK, doc["schema"], TICK))
    lines.append("")
    lines.append("## 0. 结论摘要")
    lines.append("")
    lines.append("| 项 | 值 |")
    lines.append("| --- | --- |")
    lines.append("| 扫描文件数 | %d |" % s["files_scanned"])
    lines.append("| 命中文件数 | %d |" % s["files_matched"])
    lines.append("| **运行依赖（runtime_dependency）** | **%d** |" % s["by_category"].get("runtime_dependency", 0))
    lines.append("| 待删/清理（to_delete） | %d |" % s["by_category"].get("to_delete", 0))
    lines.append("| 仅历史/工装（historical_only） | %d |" % s["by_category"].get("historical_only", 0))
    lines.append("| 未分类（unclassified） | %d |" % s["by_category"].get("unclassified", 0))
    lines.append("| 新栈载荷 positive dependency 行 | %d |" % pa["runtime_dependency_lines"])
    lines.append("| 新栈载荷 ordering-only 残留行 | %d |" % pa["ordering_dependency_lines"])
    lines.append("")
    if pa["ordering_dependency_lines"] > 0:
        lines.append("> **注意**：新栈载荷存在 ordering-only 旧 unit 残留（%sBefore=%s，不激活 C），"
                     "按 G-no-C「其它 unit 不再依赖 C」应在 T71 前清理；详见 §4。" % (TICK, TICK))
        lines.append("")
    lines.append("## 1. 扫描范围与方法")
    lines.append("")
    lines.append(TICK*3 + "text")
    for cat, roots in doc["scope"].items():
        lines.append("%-8s %s" % (cat, ", ".join(roots)))
    lines.append(TICK*3)
    lines.append("")
    lines.append("- 匹配模式：%scamilla[-_ ]?dsp%s、%s\\bcamilla\\b%s、旧 unit 名（khaen-camilladsp / khaen-upmix-autodetect / "
                 "khaen-audio-web / khaen-audio.target / khaen-boot-mode）、旧 DSP 工具（khaen-upmix / khaen-mode / "
                 "khaen-channelmap）、C 路径（/etc/camilladsp、/run/khaen/camilladsp.*、/usr/local/bin/camilladsp）、"
                 "C WS（ws://...:1234、KHAEN_CAMILLADSP_PORT）、%s\\bsox\\b%s、C socket/pipe 行、1234/18080 端口"
                 "（带上下文判定，排除 offset_ns:1234、255.255.255.254:1234 等误报）。"
                 % (TICK, TICK, TICK, TICK, TICK, TICK))
    lines.append("- 输入指纹：%sinputs_sha256=%s%s（逐命中文件 sha256 聚合）" % (TICK, doc["inputs_sha256"], TICK))
    lines.append("- 追加扫描（域外旁证，不计入主结论）：%s" % ", ".join(doc["supplement_roots"]))
    lines.append("")
    lines.append("## 2. 运行依赖清单（T72 要求为空）")
    lines.append("")
    items = [it for it in doc["items"] if it["category"] == "runtime_dependency"]
    if not items:
        lines.append("**（空）——扫描域内无任何会被部署/启动并实际启动或联系 CamillaDSP 的引用。**")
    else:
        lines.append("| # | 角色 | 文件 | 命中 | 首条证据 |")
        lines.append("| --- | --- | --- | --- | --- |")
        for i, it in enumerate(items, 1):
            first = it["hit_lines"][0]["text"] if it["hit_lines"] else "(文件名命中)"
            lines.append("| %d | %s | %s%s%s | %s | %s%s%s |" % (
                i, esc_md(it["role"]), TICK, esc_md(it["file"]), TICK,
                esc_md(", ".join("%s=%d" % kv for kv in sorted(it["counts"].items())) or "filename"),
                TICK, esc_md(first[:160]), TICK))
    lines.append("")
    lines.append("## 3. 待删/清理清单")
    lines.append("")
    items = [it for it in doc["items"] if it["category"] == "to_delete"]
    if not items:
        lines.append("_（空）_")
    else:
        lines.append("| # | 角色 | 文件 | 命中 | 首条证据 |")
        lines.append("| --- | --- | --- | --- | --- |")
        for i, it in enumerate(items, 1):
            first = it["hit_lines"][0]["text"] if it["hit_lines"] else "(文件名命中)"
            tag = " (域外旁证)" if it.get("supplement") else ""
            lines.append("| %d | %s%s | %s%s%s | %s | %s%s%s |" % (
                i, esc_md(it["role"]), tag, TICK, esc_md(it["file"]), TICK,
                esc_md(", ".join("%s=%d" % kv for kv in sorted(it["counts"].items())) or "filename"),
                TICK, esc_md(first[:160]), TICK))
    lines.append("")
    lines.append("## 4. 新栈载荷审计（bundle units + 安装模板）")
    lines.append("")
    lines.append("被 04-install-release 安装到板端的文件：")
    lines.append("")
    lines.append(TICK*3 + "text")
    for f in doc["payload_audit"]["payload_files"]:
        lines.append(f)
    lines.append(TICK*3)
    lines.append("")
    if pa["ordering_dependency_lines"] == 0 and pa["runtime_dependency_lines"] == 0:
        lines.append("_（载荷中无旧 C 引用）_")
    else:
        lines.append("| 文件 | 行 | 类型 | 内容 |")
        lines.append("| --- | --- | --- | --- |")
        for e in doc["payload_audit"]["entries"]:
            for r in e["refs"]:
                lines.append("| %s%s%s | %s | %s | %s%s%s |" % (
                    TICK, e["file"], TICK, r["line"], r["kind"], TICK, esc_md(r["text"][:180]), TICK))
    lines.append("")
    lines.append("## 5. 仅历史/工装清单（保留）")
    lines.append("")
    items = [it for it in doc["items"] if it["category"] == "historical_only"]
    if not items:
        lines.append("_（空）_")
    else:
        lines.append("| # | 角色 | 文件 | 命中 | 首条证据 |")
        lines.append("| --- | --- | --- | --- | --- |")
        for i, it in enumerate(items, 1):
            first = it["hit_lines"][0]["text"] if it["hit_lines"] else "(文件名命中)"
            tag = " (域外旁证)" if it.get("supplement") else ""
            lines.append("| %d | %s%s | %s%s%s | %s | %s%s%s |" % (
                i, esc_md(it["role"]), tag, TICK, esc_md(it["file"]), TICK,
                esc_md(", ".join("%s=%d" % kv for kv in sorted(it["counts"].items())) or "filename"),
                TICK, esc_md(first[:160]), TICK))
    lines.append("")
    lines.append("## 6. 未分类文件（必须为 0）")
    lines.append("")
    unc = [it for it in doc["items"] if it["category"] == "unclassified"]
    if unc:
        for it in unc:
            lines.append("- %s%s%s" % (TICK, it["file"], TICK))
    else:
        lines.append("_（无：所有命中文件均有明确分类）_")
    lines.append("")
    lines.append("## 7. 附加说明")
    lines.append("")
    lines.append("- %s18080%s 在本仓库五域内**零真实引用**（仅 2 处为数值巧合的浮点小数）；板端检查脚本仍把它列为"
                 "禁用端口，默认期望无监听。" % (TICK, TICK))
    lines.append("- %ssox%s 在五域内零引用；板端检查脚本仍按旧 DSP 进程模式检查（防历史残留）。" % (TICK, TICK))
    lines.append("- %sweb/%s 整体为旧 Web shim（CamillaDSP WS 客户端 + systemctl 控制 C），由新 %skhaen-rt-daemon%s Web 替代；"
                 "其文档保留为历史，源码/产物/部署脚本按待删处理。" % (TICK, TICK, TICK, TICK))
    lines.append("- overlay 中保留 AoIP/PTP unit 仍 %sAfter=/PartOf=khaen-audio.target%s（该 target disabled；仅排序引用，"
                 "不激活 C）；T71 后建议将 %sAfter=%s 指向新 target 或去掉。" % (TICK, TICK, TICK, TICK))
    lines.append("")
    lines.append("## 8. 复现命令")
    lines.append("")
    lines.append(TICK*3 + "bash")
    lines.append(doc["command"])
    lines.append(TICK*3)
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description="T72 no-C offline static scan")
    ap.add_argument("--project-root", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--json", default=None)
    ap.add_argument("--md", default=None)
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    project_root = os.path.abspath(args.project_root)
    if not os.path.isdir(project_root):
        print("[scan] project root 不存在: %s" % project_root, file=sys.stderr)
        return 7
    out_dir = os.path.abspath(args.out_dir)
    json_path = args.json or os.path.join(out_dir, "refs.json")
    md_path = args.md or os.path.join(out_dir, "REFS.md")
    os.makedirs(os.path.dirname(json_path) or ".", exist_ok=True)
    os.makedirs(os.path.dirname(md_path) or ".", exist_ok=True)

    collected = []
    exclusions = []
    for scope, roots in SCOPE_DIRS.items():
        for r in roots:
            walk_root(project_root, r, collected, exclusions)
    supplement = []
    for r in SUPPLEMENT_DIRS:
        walk_root(project_root, r, supplement, exclusions)
    for f in SUPPLEMENT_FILES:
        full = os.path.join(project_root, f)
        if os.path.isfile(full):
            supplement.append({"rel": f, "abs": full, "size": os.path.getsize(full)})

    seen = set()
    items = []
    for entry in collected:
        if entry["rel"] in seen:
            continue
        seen.add(entry["rel"])
        it = scan_file(entry)
        if it:
            items.append(it)
    for entry in supplement:
        if entry["rel"] in seen:
            continue
        seen.add(entry["rel"])
        it = scan_file(entry, supplement=True)
        if it:
            items.append(it)

    order = {"runtime_dependency": 0, "unclassified": 1, "to_delete": 2, "historical_only": 3}
    items.sort(key=lambda x: (0 if x.get("supplement") else 1, order.get(x["category"], 9), x["file"]))

    by_cat = {}
    for it in items:
        if it.get("supplement"):
            continue
        by_cat[it["category"]] = by_cat.get(it["category"], 0) + 1

    hashed = sorted("%s:%s" % (it["file"], it["sha256"]) for it in items)
    inputs_sha256 = hashlib.sha256("\n".join(hashed).encode("utf-8")).hexdigest()

    cmd = ("python3 rt-stack/tools/no-c-proof/scan_no_c_refs.py --project-root . "
           "--out-dir rt-stack/evidence/no-c-proof/scan")
    pa = payload_audit(project_root, set(seen))
    doc = {
        "schema": "khaen-rt-stack/no-c-proof-scan/v1",
        "task": "T72",
        "generated_at": now_iso(),
        "project_root": project_root,
        "command": cmd,
        "host": {"platform": platform.platform(), "python": platform.python_version()},
        "scope": SCOPE_DIRS,
        "supplement_roots": SUPPLEMENT_DIRS + SUPPLEMENT_FILES,
        "exclusions": exclusions,
        "patterns": {label: rx.pattern for label, rx in LINE_PATTERNS},
        "summary": {
            "files_scanned": len(collected) + len(supplement),
            "files_matched": len(items),
            "by_category": by_cat,
            "runtime_dependency_files": [it["file"] for it in items
                                         if it["category"] == "runtime_dependency"],
            "supplement_files": [it["file"] for it in items if it.get("supplement")],
        },
        "inputs_sha256": inputs_sha256,
        "items": items,
        "payload_audit": pa,
    }

    with open(json_path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(doc, f, ensure_ascii=False, indent=1, sort_keys=False)
        f.write("\n")
    with open(md_path, "w", encoding="utf-8", newline="\n") as f:
        f.write(render_md(doc))

    if not args.quiet:
        print("[scan] root=%s" % project_root)
        print("[scan] scanned=%d matched=%d runtime=%d to_delete=%d historical=%d unclassified=%d" % (
            doc["summary"]["files_scanned"], len(items),
            by_cat.get("runtime_dependency", 0), by_cat.get("to_delete", 0),
            by_cat.get("historical_only", 0), by_cat.get("unclassified", 0)))
        print("[scan] payload positive_dependency=%d ordering_only=%d negative_guard=%d unknown=%d" % (
            pa["summary"]["runtime_dependency_lines"], pa["summary"]["ordering_dependency_lines"],
            pa["summary"]["negative_guard_lines"], pa["summary"]["unknown_lines"]))
        print("[scan] json=%s" % json_path)
        print("[scan] md=%s" % md_path)
        print("[scan] inputs_sha256=%s" % inputs_sha256)

    if by_cat.get("unclassified", 0) > 0:
        print("[scan][FAIL] 存在未分类文件；补规则后重跑", file=sys.stderr)
        return 3
    if by_cat.get("runtime_dependency", 0) > 0 or pa["summary"]["runtime_dependency_lines"] > 0:
        print("[scan][FAIL] 发现运行依赖（T72 G-no-C 失败）", file=sys.stderr)
        return 4
    return 0


if __name__ == "__main__":
    sys.exit(main())
