#!/usr/bin/env python3
"""T28/T147 systemd 单元静态检查器（无 systemd 环境下的语法/契约检查；替代 systemd-analyze verify）。

检查：
  * 无 tab / CRLF；section 合法；未知 key 拒绝（防拼写错误）；
  * .service 需要 [Service] Type/ExecStart 与 [Install] WantedBy；.target 需要 [Unit] 与 [Install]；
  * ExecStart 首 token 必须是绝对路径（允许前缀 -/@/+/!）；Type/Restart 枚举合法；
  * khaen-rt-host.service 必须是 RT 门槛（LimitRTPRIO=99 + LimitMEMLOCK=infinity）；
  * khaen-rt-stack.target 必须 Wants/After 自研 host+daemon（F-18/T28-P0-1：否则只 start target 拉不起 host）；
  * 新栈 unit 不得依赖/启动旧 C（camilladsp、khaen-upmix-autodetect、khaen-audio.target、khaen-mode、
    khaen-audio-web、khaen-boot-mode）；缺省同时检查 service/ 与 deploy/config/（task-223 T72-N9：
    keeper 的 Before= 曾残留旧 unit 名，deploy/config 不能漏检）；
  * 90-safe-mute.conf.example（drop-in）只允许 [Service] 的 Environment/EnvironmentFile/ExecStart 类 key；
    F-17：必须用 EnvironmentFile 覆盖 rt-host.env 的 normal（Environment= 会被 EnvironmentFile 覆盖），
    且 env 文件（90-safe-mute.env.example）只写 KHAEN_HOST_SAFE_MODE=mute（host 实际读取的变量名）；
  * F3（2026-11-18）：host/daemon 必须有 RuntimeDirectory/LogsDirectory=khaen-rt-stack，覆盖
    ReadWritePaths 里的 /run/khaen-rt-stack、/var/log/khaen-rt-stack（否则 ProtectSystem=strict 下
    mount namespace 因目录不存在而失败）；
  * F4（2026-11-18）：host 的 DeviceAllow 必须是 char-alsa rw（ALSA 设备组），禁止 /dev/snd/* 通配
    （systemd 252 + cgroup v2 下不生效，板端实测所有 ALSA 节点 EPERM）；
  * B1（2026-11-20 板窗）：khaen-irq-affinity.service 必须带开机竞态等待——ExecStartPre 调用
    irq-affinity-pin wait（IRQ56/57 节点可能在 oneshot 启动后才注册），且 TimeoutStartSec>=90
    （覆盖 wait 默认 60s + 钉核回读余量）。

用法: check_unit_syntax.py [FILE...|DIR...]（缺省 service/） [--json OUT]
输出 JSON；exit 0=PASS，3=FAIL。
"""
import glob
import json
import os
import sys

UNIT_KEYS = {
    'Unit': {'Description', 'Documentation', 'After', 'Before', 'Wants', 'Requires', 'BindsTo',
             'PartOf', 'Conflicts', 'OnFailure', 'ConditionPathExists', 'ConditionPathExistsGlob',
             'ConditionPathIsDirectory', 'ConditionPathIsReadWrite', 'ConditionFileNotEmpty',
             'DefaultDependencies', 'StartLimitIntervalSec', 'StartLimitBurst', 'RefuseManualStart',
             'RefuseManualStop', 'AllowIsolate', 'IgnoreOnIsolate'},
    'Service': {'Type', 'ExecStart', 'ExecStartPre', 'ExecStartPost', 'ExecStop', 'ExecStopPost',
                'ExecReload', 'Restart', 'RestartSec', 'TimeoutStartSec', 'TimeoutStopSec', 'TimeoutSec',
                'KillSignal', 'KillMode', 'SendSIGKILL', 'User', 'Group', 'SupplementaryGroups',
                'Environment', 'EnvironmentFile', 'WorkingDirectory', 'StandardOutput', 'StandardError',
                'LimitRTPRIO', 'LimitMEMLOCK', 'LimitNOFILE', 'Nice', 'IOSchedulingClass',
                'CapabilityBoundingSet', 'AmbientCapabilities', 'NoNewPrivileges', 'ProtectSystem',
                'ProtectHome', 'PrivateTmp', 'ReadWritePaths', 'ReadOnlyPaths', 'InaccessiblePaths',
                'DeviceAllow', 'ProtectClock', 'ProtectKernelLogs', 'ProtectKernelTunables',
                'ProtectKernelModules', 'ProtectControlGroups', 'RestrictAddressFamilies',
                'RestrictSUIDSGID', 'RestrictNamespaces', 'LockPersonality', 'SystemCallArchitectures',
                'UMask', 'WatchdogSec', 'RemainAfterExit', 'SuccessExitStatus', 'RuntimeDirectory',
                'RuntimeDirectoryMode', 'RuntimeDirectoryPreserve', 'StateDirectory',
                'LogsDirectory', 'LogsDirectoryMode', 'TasksMax',
                'CPUSchedulingPolicy', 'CPUSchedulingPriority',
                'CPUAffinity', 'MemoryMax', 'OOMPolicy', 'UMask', 'ProcSubset', 'IPAddressAllow'},
    'Install': {'WantedBy', 'RequiredBy', 'Alias', 'Also', 'DefaultInstance'},
}
TYPE_OK = {'simple', 'exec', 'forking', 'oneshot', 'dbus', 'notify', 'notify-reload', 'idle'}
RESTART_OK = {'no', 'on-success', 'on-failure', 'on-abnormal', 'on-abort', 'on-watchdog', 'always'}
OLD_C = ('camilladsp', 'khaen-upmix-autodetect', 'khaen-audio.target', 'khaen-mode',
         'khaen-audio-web', 'khaen-boot-mode')


def parse(path):
    problems = []
    section = None
    entries = []
    raw = open(path, 'rb').read()
    if b'\r' in raw:
        problems.append('含 CRLF（systemd unit 要求 LF）')
    text = raw.decode('utf-8', 'replace')
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1
        if '\t' in line:
            problems.append(f'{i}: 含 TAB（systemd 不接受）')
        s = line.strip()
        if not s or s.startswith('#') or s.startswith(';'):
            continue
        while s.endswith('\\') and i < len(lines):
            s = s[:-1].rstrip() + ' ' + lines[i].strip()
            i += 1
        if s.startswith('[') and s.endswith(']'):
            section = s[1:-1]
            if section not in UNIT_KEYS:
                problems.append(f'{i}: 未知 section [{section}]')
            continue
        if '=' not in s:
            problems.append(f'{i}: 非 key=value 行: {s[:80]}')
            continue
        k, v = s.split('=', 1)
        k = k.strip()
        if section is None:
            problems.append(f'{i}: 在任何 [section] 之前的 key: {k}')
            continue
        if section in UNIT_KEYS and k not in UNIT_KEYS[section]:
            problems.append(f'{i}: [{section}] 未知 key: {k}')
        entries.append((section, k, v, i))
    return problems, entries


def parse_env(path):
    """safe-mute env：VAR=VALUE 行（允许注释/空行）；返回 [( 'env', KEY, VALUE, lineno )]。"""
    problems, entries = [], []
    for i, line in enumerate(open(path, encoding='utf-8', errors='replace'), start=1):
        s = line.strip()
        if not s or s.startswith('#') or s.startswith(';'):
            continue
        if '=' not in s:
            problems.append(f'{i}: env 非 KEY=VALUE 行: {s[:80]}')
            continue
        k, v = s.split('=', 1)
        entries.append(('env', k.strip(), v.strip(), i))
    return problems, entries


def check(path):
    name = os.path.basename(path)
    if name.endswith('.env.example'):
        problems, entries = parse_env(path)
    else:
        problems, entries = parse(path)
    sections = {s for s, _k, _v, _i in entries}
    is_dropin = '/service.d/' in path.replace(os.sep, '/') or name.endswith('.conf.example')
    is_env_dropin = name.endswith('.env.example')
    is_target = name.endswith('.target')
    is_service = name.endswith('.service')
    values = {}
    for s, k, v, _i in entries:
        values.setdefault((s, k), []).append(v.strip())
    if is_env_dropin:
        # F-17：safe-mute env 只允许 host 实际读取的 KHAEN_HOST_SAFE_MODE（不得写 KHAEN_RT_HOST_SAFE_MODE）。
        env_vars = {}
        for (s, k), vs in values.items():
            if s != 'env':
                problems.append(f'env 文件不允许 section: [{s}]')
                continue
            for v in vs:
                if k == 'KHAEN_HOST_SAFE_MODE':
                    env_vars[k] = v
                else:
                    problems.append(f'env 文件出现未许可变量 {k}（只允许 KHAEN_HOST_SAFE_MODE）')
        if env_vars.get('KHAEN_HOST_SAFE_MODE') != 'mute':
            problems.append('env 文件必须写 KHAEN_HOST_SAFE_MODE=mute')
        return {'file': name, 'kind': 'envdropin', 'ok': not problems, 'problems': problems}
    if is_dropin:
        if any(s != 'Service' for s in sections):
            problems.append('drop-in 只允许 [Service] section')
        allowed = {'Environment', 'EnvironmentFile', 'ExecStart', 'ExecStartPre', 'ExecStartPost',
                   'Restart', 'RestartSec'}
        for (_s, k), _vs in values.items():
            if k not in allowed:
                problems.append(f'drop-in 不允许 key: {k}')
        if name == '90-safe-mute.conf.example':
            for v in values.get(('Service', 'Environment'), []):
                if v.startswith('KHAEN_'):
                    problems.append(f'F-17：drop-in 不能用 Environment= 覆盖 env 文件（{v}）；'
                                    'systemd 语义下 EnvironmentFile= 优先，请用 EnvironmentFile=')
            ef = values.get(('Service', 'EnvironmentFile'), [])
            if not any('90-safe-mute.env' in v for v in ef):
                problems.append('F-17：drop-in 必须 EnvironmentFile=/etc/khaen-rt-stack/90-safe-mute.env')
            blob = '\n'.join(line for line in open(path, encoding='utf-8', errors='replace')
                              if not line.strip().startswith('#') and not line.strip().startswith(';'))
            if 'KHAEN_RT_HOST_SAFE_MODE' in blob:
                problems.append('F-17：仍出现 host 不读的 KHAEN_RT_HOST_SAFE_MODE')
        return {'file': name, 'kind': 'dropin', 'ok': not problems, 'problems': problems}
    if is_service or is_target:
        if 'Unit' not in sections:
            problems.append('缺 [Unit]')
        if 'Install' not in sections:
            problems.append('缺 [Install]')
    if is_service:
        if 'Service' not in sections:
            problems.append('缺 [Service]')
        ex = values.get(('Service', 'ExecStart'), [])
        if not ex:
            problems.append('缺 ExecStart')
        else:
            first = ex[0].split()[0] if ex[0].split() else ''
            head = first.lstrip('-@+!')
            if not head.startswith('/'):
                problems.append(f'ExecStart 必须绝对路径: {first}')
        ty = values.get(('Service', 'Type'), ['simple'])[0]
        if ty not in TYPE_OK:
            problems.append(f'Type 非法: {ty}')
        rs = values.get(('Service', 'Restart'), [])
        if rs and rs[0] not in RESTART_OK:
            problems.append(f'Restart 非法: {rs[0]}')
    if is_target and 'Service' in sections:
        problems.append('.target 不应含 [Service]')
    # RT host 门槛
    if name == 'khaen-rt-host.service':
        if values.get(('Service', 'LimitRTPRIO'), [''])[0] != '99':
            problems.append('khaen-rt-host 必须 LimitRTPRIO=99（RT 门槛）')
        if values.get(('Service', 'LimitMEMLOCK'), [''])[0] != 'infinity':
            problems.append('khaen-rt-host 必须 LimitMEMLOCK=infinity（mlockall 准入）')
        # task-192：T45 IRQ 持久化 oneshot 必须真的约束 host（Requires+After；否则 pin 失败仍启动）
        req = []
        for v in values.get(('Unit', 'Requires'), []):
            req += v.split()
        aft = []
        for v in values.get(('Unit', 'After'), []):
            aft += v.split()
        if 'khaen-irq-affinity.service' not in req:
            problems.append('khaen-rt-host 必须 Requires=khaen-irq-affinity.service（pin 失败不启动 host）')
        if 'khaen-irq-affinity.service' not in aft:
            problems.append('khaen-rt-host 必须 After=khaen-irq-affinity.service（先钉核后启动）')
    # F3/F4（2026-11-18 板端缺陷回归）：host/daemon 的 RuntimeDirectory/LogsDirectory 与 ALSA 设备白名单。
    if name in ('khaen-rt-host.service', 'khaen-rt-daemon.service'):
        rd = []
        for v in values.get(('Service', 'RuntimeDirectory'), []):
            rd += v.split()
        ld = []
        for v in values.get(('Service', 'LogsDirectory'), []):
            ld += v.split()
        rw = []
        for v in values.get(('Service', 'ReadWritePaths'), []):
            rw += v.split()
        if 'khaen-rt-stack' not in rd:
            problems.append(f'F3：{name} 必须 RuntimeDirectory=khaen-rt-stack'
                            '（ReadWritePaths=/run/khaen-rt-stack 要求目录在 namespace 建立前存在）')
        if 'khaen-rt-stack' not in ld:
            problems.append(f'F3：{name} 必须 LogsDirectory=khaen-rt-stack')
        if '/run/khaen-rt-stack' in rw and 'khaen-rt-stack' in rd:
            pass  # RuntimeDirectory 提供 /run/khaen-rt-stack
        elif '/run/khaen-rt-stack' in rw:
            problems.append(f'F3：{name} ReadWritePaths 含 /run/khaen-rt-stack 但 RuntimeDirectory 未提供')
        if '/var/log/khaen-rt-stack' in rw and 'khaen-rt-stack' not in ld:
            problems.append(f'F3：{name} ReadWritePaths 含 /var/log/khaen-rt-stack 但 LogsDirectory 未提供')
        if rd:
            modes = values.get(('Service', 'RuntimeDirectoryMode'), [])
            if modes and modes[0] != '0755':
                problems.append(f'F3：{name} RuntimeDirectoryMode 应为 0755（got {modes[0]}）')
        if ld:
            modes = values.get(('Service', 'LogsDirectoryMode'), [])
            if modes and modes[0] != '0755':
                problems.append(f'F3：{name} LogsDirectoryMode 应为 0755（got {modes[0]}）')
    if name == 'khaen-rt-host.service':
        da = []
        for v in values.get(('Service', 'DeviceAllow'), []):
            da += [v.strip()]
        if 'char-alsa rw' not in da:
            problems.append('F4：khaen-rt-host 必须 DeviceAllow=char-alsa rw（ALSA 设备组）')
        for v in da:
            if '/dev/snd' in v or '*' in v:
                problems.append(f'F4：DeviceAllow 不得用设备通配（systemd 252+cgroup v2 不生效）: {v}')
        if len([v for v in da if v]) > 1:
            problems.append(f'F4：khaen-rt-host 只应有一条 DeviceAllow（实际 {len(da)} 条）')
    # F-18/T28-P0-1：target 必须显式 Wants/After host+daemon
    if name == 'khaen-rt-stack.target':
        for key in ('Wants', 'After'):
            items = []
            for v in values.get(('Unit', key), []):
                items += v.split()
            for need in ('khaen-rt-host.service', 'khaen-rt-daemon.service'):
                if need not in items:
                    problems.append(f'target {key}= 缺 {need}（F-18：只 start target 拉不起 host/daemon）')
            # task-192：只 start target 时也必须拉起 IRQ 持久化 oneshot（不能只依赖 host 的 Requires）
            if 'khaen-irq-affinity.service' not in items:
                problems.append(f'target {key}= 缺 khaen-irq-affinity.service（task-192 IRQ 持久化）')
    # task-192：IRQ 亲和持久化 oneshot 契约（Before host + oneshot/RemainAfterExit + pin 脚本路径）
    if name == 'khaen-irq-affinity.service':
        if values.get(('Service', 'Type'), [''])[0] != 'oneshot':
            problems.append('khaen-irq-affinity.service 必须 Type=oneshot')
        if values.get(('Service', 'RemainAfterExit'), [''])[0] != 'yes':
            problems.append('khaen-irq-affinity.service 必须 RemainAfterExit=yes（启动后保持 active 供 verify 查）')
        ex = values.get(('Service', 'ExecStart'), [''])
        if not ex or '/usr/local/lib/khaen/irq-affinity-pin' not in ex[0]:
            problems.append('khaen-irq-affinity.service ExecStart 必须是 /usr/local/lib/khaen/irq-affinity-pin apply')
        before = []
        for v in values.get(('Unit', 'Before'), []):
            before += v.split()
        if 'khaen-rt-host.service' not in before:
            problems.append('khaen-irq-affinity.service 必须 Before=khaen-rt-host.service（host 启动前钉核）')
        # B1（2026-11-20 板窗）：开机竞态防护——ExecStartPre 先等依赖 IRQ 节点就绪，再由 ExecStart 钉核。
        # （v11 重启实测：oneshot ~4s 先跑，end0 IRQ56/57 节点未注册 → apply fail-closed → host 因 Requires= 不启动）
        pre = [v.strip() for v in values.get(('Service', 'ExecStartPre'), [])]
        if not any('irq-affinity-pin' in v and 'wait' in v for v in pre):
            problems.append('khaen-irq-affinity.service 必须 ExecStartPre=/usr/local/lib/khaen/irq-affinity-pin wait'
                            '（B1 开机 IRQ 竞态：IRQ56/57 节点可能晚于 oneshot 就绪）')
        tos = values.get(('Service', 'TimeoutStartSec'), [''])
        try:
            tos_v = int(tos[0])
        except (ValueError, IndexError):
            tos_v = 0
        if tos_v < 90:
            problems.append('khaen-irq-affinity.service 必须显式 TimeoutStartSec>=90'
                            '（wait 默认 60s + 钉核回读余量；B1）')
    # 旧 C 依赖/启动禁止（注释已被 parse 跳过）
    blob = '\n'.join(line for line in open(path, encoding='utf-8', errors='replace'))
    for token in OLD_C:
        for line in blob.splitlines():
            s = line.strip()
            if not s or s.startswith('#') or s.startswith(';'):
                continue
            if s.startswith("Condition") and token in s:
                continue  # 防误启守卫可显式引用旧 C 入口文件
            if token in s:
                problems.append(f'新栈 unit 出现旧 C 引用 {token!r}: {s[:120]}')
                break
    return {'file': name, 'kind': 'target' if is_target else 'service', 'ok': not problems, 'problems': problems}


def main(argv):
    paths = []
    out = None
    i = 1
    while i < len(argv):
        a = argv[i]
        if a == '--json' and i + 1 < len(argv):
            out = argv[i + 1]
            i += 2
            continue
        if not a.startswith('--'):
            if os.path.isdir(a):
                paths += glob.glob(os.path.join(a, '*.service')) + glob.glob(os.path.join(a, '*.target'))
                paths += glob.glob(os.path.join(a, '*.d', '*.conf.example'))
                paths += glob.glob(os.path.join(a, '*.d', '*.env.example'))
            else:
                paths.append(a)
        i += 1
    if not paths:
        here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        svc = os.path.join(os.path.dirname(here), 'service')
        paths = glob.glob(os.path.join(svc, '*.service')) + glob.glob(os.path.join(svc, '*.target'))
        paths += glob.glob(os.path.join(svc, '*.d', '*.conf.example'))
        paths += glob.glob(os.path.join(svc, '*.d', '*.env.example'))
        # task-223/T72-N9: deploy/config units are installed to /etc/systemd/system too;
        # they must satisfy the same no-old-C contract (keeper Before= regression).
        cfg = os.path.join(here, 'config')
        paths += glob.glob(os.path.join(cfg, '*.service'))
        paths += glob.glob(os.path.join(cfg, '*.target'))
    results = [check(p) for p in sorted(paths)]
    ok = all(r['ok'] for r in results)
    report = {'ok': ok, 'checked': len(results), 'results': results}
    text = json.dumps(report, ensure_ascii=False, indent=1)
    print(text)
    if out:
        open(out, 'w', encoding='utf-8').write(text + '\n')
    return 0 if ok else 3


if __name__ == '__main__':
    sys.exit(main(sys.argv))
