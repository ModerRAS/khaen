#!/usr/bin/env python3
"""T28 独立检查器：rollback 文件/回退契约（WBS T28 计划验收命令 rt-check rollback-file 的等价离线检查）。

本检查器不信任脚本自报，独立做三层校验：
  A last-good.json（可选）：schema=tspi-rt-stack/last-good/v1、c_policy=never-camilladsp、
    previous 仍是自研 release（不出现 camilladsp）、release_dir 存在性（--require-release）。
  B deploy/bin|lib 源码：禁止任何启动/使能/解除屏蔽 CamillaDSP/upmix-autodetect 或 `tspi-mode dante|aes67`
    的语句（注释除外）；rollback 只允许 last-good/previous/mute 三个目标。
  C service/ 单元草案：不得依赖/启动旧 C（camilladsp、tspi-upmix-autodetect、tspi-audio.target）。
用法: check_rollback_file.py --deploy DIR [--service DIR] [--last-good FILE] [--require-release]
输出 JSON；exit 0=PASS，3=FAIL，2=用法错误。
"""
import json
import os
import re
import sys

FORBIDDEN = [
    (re.compile(r"^\s*(?:run\s+|sudo\s+)?systemctl\s+(?:start|restart|try-restart|reload-or-restart|enable|unmask)\b[^\n]*(?:camilladsp|tspi-upmix-autodetect)", re.I),
     "systemctl start/enable/unmask 旧 C unit"),
    (re.compile(r"^\s*(?:run\s+|sudo\s+)?tspi-mode\s+(?:dante|aes67)\b"), "调用旧 tspi-mode 开启 C 音频模式"),
    (re.compile(r"^\s*ExecStart\s*=\s*\S*camilladsp", re.I), "ExecStart 启动 camilladsp"),
]


def scan_shell(paths):
    hits = []
    for p in paths:
        try:
            lines = open(p, encoding='utf-8', errors='replace').read().splitlines()
        except OSError as e:
            hits.append({'file': p, 'line': 0, 'rule': '不可读', 'text': str(e)})
            continue
        for i, line in enumerate(lines, 1):
            s = line.strip()
            if not s or s.startswith('#'):
                continue
            code = s.split(" #", 1)[0]
            for rx, why in FORBIDDEN:
                if rx.search(code):
                    hits.append({'file': os.path.basename(p), 'line': i, 'rule': why, 'text': s[:200]})
    return hits


def scan_units(paths):
    hits = []
    dep = re.compile(r"^(?:After|Before|Wants|Requires|BindsTo|PartOf|Conflicts|OnFailure)\s*=")
    forbid = re.compile(r"(camilladsp|tspi-upmix-autodetect|tspi-audio\.target)", re.I)
    for p in paths:
        try:
            for i, line in enumerate(open(p, encoding='utf-8', errors='replace'), 1):
                s = line.strip()
                if not s or s.startswith('#'):
                    continue
                if dep.match(s) and forbid.search(s):
                    hits.append({'file': os.path.basename(p), 'line': i, 'rule': 'unit 依赖旧 C', 'text': s[:200]})
                if s.startswith('ExecStart') and 'camilladsp' in s.lower():
                    hits.append({'file': os.path.basename(p), 'line': i, 'rule': 'ExecStart 旧 C', 'text': s[:200]})
        except OSError as e:
            hits.append({'file': p, 'line': 0, 'rule': '不可读', 'text': str(e)})
    return hits


def check_last_good(path, require_release):
    problems = []
    try:
        d = json.load(open(path, encoding='utf-8'))
    except (OSError, json.JSONDecodeError) as e:
        return ['last-good 不可读/非法: ' + str(e)]
    if d.get('schema') != 'tspi-rt-stack/last-good/v1':
        problems.append('schema 不符')
    if d.get('c_policy') != 'never-camilladsp':
        problems.append('c_policy 必须是 never-camilladsp')
    for k in ('release_id', 'release_dir', 'binary_sha256', 'units_sha256', 'verified_at'):
        if not d.get(k):
            problems.append('缺字段 ' + k)
    blob = json.dumps(d, ensure_ascii=False).lower()
    # c_policy=never-camilladsp 是允许的固定声明；其余出现 camilladsp 即视为 C 回退
    rest = blob.replace('never-camilladsp', '')
    if 'camilladsp' in rest:
        problems.append('last-good 内容出现 camilladsp（禁止 C 回退）')
    prev = d.get('previous')
    if prev is not None:
        if not isinstance(prev, dict):
            problems.append('previous 必须是对象或 null')
        elif 'camilladsp' in json.dumps(prev, ensure_ascii=False).lower():
            problems.append('previous 指向旧 C')
    if require_release:
        rd = d.get('release_dir') or ''
        if not os.path.isdir(rd):
            problems.append('release_dir 不存在: ' + rd)
        elif not os.path.isfile(os.path.join(rd, 'manifest.json')):
            problems.append('release_dir 缺 manifest.json: ' + rd)
    return problems


def main(argv):
    deploy = service = lg = None
    require_release = '--require-release' in argv
    for i, a in enumerate(argv):
        if a == '--deploy' and i + 1 < len(argv):
            deploy = argv[i + 1]
        elif a == '--service' and i + 1 < len(argv):
            service = argv[i + 1]
        elif a == '--last-good' and i + 1 < len(argv):
            lg = argv[i + 1]
    if not deploy:
        print(json.dumps({'ok': False, 'error': '用法: check_rollback_file.py --deploy DIR [--service DIR] [--last-good FILE] [--require-release]'}))
        return 2
    shell_paths = []
    for sub in ('bin', 'lib'):
        d = os.path.join(deploy, sub)
        if os.path.isdir(d):
            shell_paths += [os.path.join(d, n) for n in sorted(os.listdir(d)) if n.endswith('.sh')]
    unit_paths = []
    if service and os.path.isdir(service):
        for root, _dirs, names in os.walk(service):
            for n in names:
                if n.endswith(('.service', '.target')):
                    unit_paths.append(os.path.join(root, n))
    hits = scan_shell(shell_paths) + scan_units(unit_paths)
    lg_problems = check_last_good(lg, require_release) if lg else []
    rb = os.path.join(deploy, 'bin', '08-rollback.sh')
    rb_ok = False
    if os.path.isfile(rb):
        text = open(rb, encoding='utf-8', errors='replace').read()
        rb_ok = all(k in text for k in ('--to-last-good', '--to-previous', '--mute', 'never-camilladsp'))
        if not rb_ok:
            hits.append({'file': '08-rollback.sh', 'line': 0, 'rule': 'rollback 模式契约缺失',
                         'text': '需要 last-good/previous/mute + never-camilladsp'})
    ok = not hits and not lg_problems
    print(json.dumps({
        'ok': ok,
        'checked_scripts': len(shell_paths),
        'checked_units': len(unit_paths),
        'last_good_checked': bool(lg),
        'violations': hits,
        'last_good_problems': lg_problems,
        'rollback_modes_present': rb_ok,
    }, ensure_ascii=False))
    return 0 if ok else 3


if __name__ == '__main__':
    sys.exit(main(sys.argv))
