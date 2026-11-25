#!/usr/bin/env python3
"""T28/T147 旧 C/旧音频栈 inventory 生成器（硬编码 hash 清单的现状 + 板端 UNKNOWN 明确列出）。

输入：rt-stack/evidence/baseline/baseline-manifest.json（T01 冻结 80 个 installed payload）+ 当前 repo 字节。
输出：evidence/migration-plan/inventory/expected-old-c-hashes.json（+ 可选 Markdown 表）。

要点（诚实口径）：
  * repo 侧可算的 sha256 现在重算（不抄 T01 数字）：repo_sha256；T01 冻结值单独列 baseline_sha256；
  * **板端实际 hash 仍是 UNKNOWN**（T01 未触板）；T71 必须先跑 00-snapshot-old-c.sh 采集 board hash；
  * disposition 只分类动作，不代表已执行；本文件不含任何板端命令执行。
用法: gen_expected_inventory.py --baseline <manifest.json> --project-root <tspi-audio-dsp> --out <json> [--md-out <md>]
"""
import hashlib
import json
import os
import sys
import datetime

SCHEMA = 'tspi-rt-stack/old-c-inventory/v1'

# 分类规则（按板端 installed_path 前缀/文件名）
RULES = [
    ('RETIRE_C_PROCESS', ['/usr/local/bin/camilladsp', '/etc/camilladsp/', '/etc/systemd/system/tspi-camilladsp.service',
                          '/etc/systemd/system/tspi-upmix-autodetect.service']),
    ('REPLACE_SHIM', ['/usr/local/bin/khaen-audio-web', '/usr/local/bin/tspi-upmix', '/usr/local/bin/tspi-mode',
                      '/etc/systemd/system/khaen-audio-web.service', '/etc/systemd/system/tspi-audio.target',
                      '/etc/systemd/system/tspi-boot-mode.service', '/etc/khaen-audio-web/']),
    ('RETAIN_AOIP', ['/usr/local/bin/statime', '/usr/local/bin/inferno2pipe', '/usr/local/bin/aes67-daemon',
                     '/usr/sbin/ptp4l', '/usr/sbin/phc2sys', '/usr/sbin/pmc',
                     '/usr/lib/aarch64-linux-gnu/alsa-lib/', '/lib/modules/', '/etc/statime/',
                     '/etc/aes67-daemon/', '/etc/ptp4l-master-aes67.conf', '/etc/asound.conf',
                     '/usr/local/lib/tspi/']),
    ('KEEP_DATA', ['/etc/tspi-audio/layout-presets.yml', '/etc/tspi-audio/upmix/', '/etc/tspi-audio/profiles/',
                   '/etc/default/tspi-audio', '/etc/modprobe.d/', '/etc/security/', '/etc/udev/', '/etc/tmpfiles.d/',
                   '/usr/share/alsa/ucm2/', '/usr/local/bin/README.md', '/usr/local/bin/tspi-channelmap']),
]
ACTION = {
    'RETIRE_C_PROCESS': 'T71: stop+disable+mask systemd unit；进程必须为空；文件保留审计，禁止作为回退',
    'REPLACE_SHIM': 'T71: stop+disable（旧 Web/WS shim/旧编排入口）；由新 daemon/Web 替代',
    'RETAIN_AOIP': 'P1 保留（compat A11，新 control 后续接管）；T71 窗口内必须 inactive',
    'KEEP_DATA': '保留数据/环境；layout-presets 是新 compat 只读真相源，旧 upmix profiles 不参与 P1',
}

# baseline 未覆盖但必须快照的板端路径（S0-10 子集 + 老 Web 配置）
EXTRA_CAPTURE = [
    '/usr/local/bin/camilladsp', '/usr/local/bin/khaen-audio-web', '/usr/local/bin/tspi-mode',
    '/usr/local/bin/tspi-channelmap', '/usr/local/bin/tspi-upmix', '/usr/local/bin/tspi-ptp-status',
    '/usr/local/bin/statime', '/usr/local/bin/inferno2pipe', '/usr/local/bin/aes67-daemon',
    '/usr/sbin/ptp4l', '/usr/sbin/phc2sys', '/usr/sbin/pmc',
    '/usr/local/lib/tspi/lib.sh', '/usr/local/lib/tspi/ptp-status-hook', '/usr/local/lib/tspi/ucm-backport-mk5.sh',
    '/usr/lib/aarch64-linux-gnu/alsa-lib/libasound_module_pcm_inferno.so',
    '/etc/khaen-audio-web/config.toml', '/etc/khaen-audio-web/auth.json',
    '/run/tspi/camilladsp.yml', '/run/tspi/camilladsp.env', '/etc/tspi/last-mode', '/run/tspi/mode',
]


def classify(installed):
    for cls, prefixes in RULES:
        for p in prefixes:
            if p.endswith('/') and installed.startswith(p):
                return cls
            if installed == p:
                return cls
    return 'KEEP_DATA'


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for c in iter(lambda: f.read(1 << 20), b''):
            h.update(c)
    return h.hexdigest()


def main(argv):
    baseline = root = out = md_out = None
    i = 1
    while i < len(argv):
        a = argv[i]
        if a == '--baseline':
            baseline = argv[i + 1]; i += 2
        elif a == '--project-root':
            root = argv[i + 1]; i += 2
        elif a == '--out':
            out = argv[i + 1]; i += 2
        elif a == '--md-out':
            md_out = argv[i + 1]; i += 2
        else:
            i += 1
    if not baseline or not root or not out:
        print(json.dumps({'ok': False, 'error': '用法: gen_expected_inventory.py --baseline JSON --project-root DIR --out JSON [--md-out MD]'}))
        return 2
    d = json.load(open(baseline, encoding='utf-8'))
    entries = []
    for e in d['entries']:
        ip = e.get('installed_path')
        if not ip:
            continue
        repo_path = e.get('path')
        fp = os.path.join(root, repo_path.replace('/', os.sep))
        repo_sha = sha256_file(fp) if os.path.isfile(fp) else None
        cls = classify(ip)
        entries.append({
            'id': e.get('id'),
            'repo_path': repo_path,
            'installed_path': ip,
            'repo_exists': os.path.isfile(fp),
            'repo_size': os.path.getsize(fp) if os.path.isfile(fp) else None,
            'repo_sha256': repo_sha,
            'baseline_sha256': e.get('sha256'),
            'match_baseline': (repo_sha == e.get('sha256')) if repo_sha and e.get('sha256') else None,
            'board_sha256': 'UNKNOWN',
            'board_hash_source': 'T01 未触板；需 T71 00-snapshot-old-c.sh 采集',
            'disposition': cls,
            'action': ACTION[cls],
        })
    covered = {e['installed_path'] for e in entries}
    extra = []
    for p in EXTRA_CAPTURE:
        if p in covered:
            continue
        extra.append({'installed_path': p, 'board_sha256': 'UNKNOWN', 'disposition': classify(p),
                      'action': ACTION[classify(p)]})
    # 冲突：repo 字节与 T01 冻结值不一致（说明 repo 在本任务前已变，如实列出）
    mismatches = [e for e in entries if e['match_baseline'] is False]
    by_class = {}
    for e in entries + extra:
        by_class[e['disposition']] = by_class.get(e['disposition'], 0) + 1
    result = {
        'schema': SCHEMA,
        'generated_at': datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec='seconds'),
        'generated_by': 'deploy/tests/gen_expected_inventory.py（只读 repo + T01 manifest；未触板）',
        'sources': {
            'baseline_manifest': os.path.relpath(baseline, root).replace(os.sep, '/'),
            'project_root': root,
        },
        'board_hash_policy': 'NEVER_CLAIM_BOARD_HASH_WITHOUT_SNAPSHOT：board_sha256 一律 UNKNOWN，直到 T71 授权窗口采集',
        'counts': {'baseline_installed_entries': len(entries), 'extra_capture_paths': len(extra), 'by_disposition': by_class,
                   'repo_baseline_mismatch': len(mismatches)},
        'entries': entries,
        'extra_capture': extra,
        'board_capture_commands': [
            'sudo /path/to/rt-stack/deploy/bin/00-snapshot-old-c.sh --live --out /var/lib/tspi-rt-stack/evidence',
            '# 等价的逐文件 hash（只读）：sha256sum <每个 installed_path>；stat -c "%n %s %a %U:%G %y" <path>',
            'systemctl list-unit-files "tspi-*" --no-pager; systemctl list-units "tspi-*" --all --no-pager',
            'pgrep -a -f "camilladsp|tspi-upmix"; ss -ltnp | grep -E ":(80|1234|8080)\\b"',
            'cat /proc/asound/cards /proc/asound/devices; ls -l /dev/snd',
        ],
        'mismatches': [{'id': m['id'], 'repo_path': m['repo_path'], 'repo_sha256': m['repo_sha256'],
                        'baseline_sha256': m['baseline_sha256']} for m in mismatches],
    }
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    open(out, 'w', encoding='utf-8').write(json.dumps(result, ensure_ascii=False, indent=1) + '\n')
    if md_out:
        rows = ['| disposition | 条数 |', '| --- | --- |']
        for k in sorted(by_class):
            rows.append(f'| {k} | {by_class[k]} |')
        key = [e for e in entries if e['disposition'] in ('RETIRE_C_PROCESS', 'REPLACE_SHIM')]
        kr = ['| installed_path | disposition | repo_sha256（本机重算） | T01 baseline | board |',
              '| --- | --- | --- | --- | --- |']
        for e in key:
            kr.append(f"| {e['installed_path']} | {e['disposition']} | {(e['repo_sha256'] or 'N/A')[:16]} | {(e['baseline_sha256'] or '')[:16]} | UNKNOWN |")
        md = ['# 旧 C/旧音频栈 inventory（自动生成）', '',
              f"生成时间：{result['generated_at']}；来源：{result['sources']['baseline_manifest']}。", '',
              '> board hash 一律 UNKNOWN：T01 未触板；T71 授权窗口内用 00-snapshot-old-c.sh 采集后回填。', '',
              f"- installed payload 条目：{len(entries)}",
              f"- 额外必须快照路径：{len(extra)}",
              f"- repo 与 T01 冻结不一致：{len(mismatches)}", '',
              '## disposition 汇总', ''] + rows + ['', '## 退役/替换关键项（repo 期望值，非板端实测）', ''] + kr + ['']
        open(md_out, 'w', encoding='utf-8').write('\n'.join(md))
    print(json.dumps({'ok': True, 'out': out, 'entries': len(entries), 'extra': len(extra),
                      'by_disposition': by_class, 'repo_baseline_mismatch': len(mismatches)}, ensure_ascii=False))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
