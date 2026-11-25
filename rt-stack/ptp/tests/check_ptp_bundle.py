#!/usr/bin/env python3
"""KHAEN PTP bundle static checker (no root, no ptp4l execution required).

Validates the PTP grandmaster bundle/source layout:
  * units: ExecStart wrappers, target Wants/After, conflicts against slave-only PTP owners,
    master Requires=khaen-ptp-phc2sys.service;
  * env template: allowed KHAEN_PTP_* keys only, valid values;
  * ptp4l configs: [global] present, required GM keys/values, no inline comments (linuxptp parser
    rejects them), no deprecated slaveOnly/masterOnly keys, transport matches the file variant,
    uds_address under /run/khaen-ptp;
  * lib wrappers: POSIX sh shebang, reference the bundled bin path, no CRLF.

Usage: check_ptp_bundle.py <dir> [--json OUT]
  <dir> may be rt-stack/ptp (source layout) or a built bundle (units/ etc/ lib/).
Exit codes: 0 PASS, 2 usage, 3 FAIL.
"""
import json
import os
import re
import sys

ENV_KEYS = {
    'KHAEN_PTP_IFACE', 'KHAEN_PTP_CONF', 'KHAEN_PTP_HW_TS', 'KHAEN_PTP_UTC_OFFSET',
    'KHAEN_PHC2SYS_SRC', 'KHAEN_PTP_PHC', 'KHAEN_PTP_EXTRA_ARGS',
}
REQUIRED_UNITS = ('khaen-ptp-master.service', 'khaen-ptp-phc2sys.service', 'khaen-ptp.target')
REQUIRED_LIB = ('ptp-lib.sh', 'ptp-master-start', 'ptp-phc2sys-start')
REQUIRED_ETC = ('ptp-master.env.example', 'ptp4l-master-udp.conf.example', 'ptp4l-master-l2.conf.example')
CONF_EXPECT = {
    'clientOnly': '0',
    'serverOnly': '1',
    'domainNumber': '0',
    'priority1': '0',
    'priority2': '0',
    'clockClass': '248',
    'delay_mechanism': 'E2E',
    'logAnnounceInterval': '0',
    'logSyncInterval': '-2',
    'ptp_dst_mac': '01:1B:19:00:00:00',
}


def read_text(path):
    raw = open(path, 'rb').read()
    return raw.decode('utf-8', 'replace'), raw


def parse_unit(path):
    vals = {}
    section = None
    for i, line in enumerate(open(path, encoding='utf-8', errors='replace'), start=1):
        s = line.strip()
        if not s or s.startswith('#') or s.startswith(';'):
            continue
        if s.startswith('[') and s.endswith(']'):
            section = s[1:-1]
            continue
        if '=' in s:
            k, v = s.split('=', 1)
            vals.setdefault((section, k.strip()), []).append(v.strip())
    return vals


def get(values, section, key):
    return values.get((section, key), [])


def words(values, section, key):
    out = []
    for v in get(values, section, key):
        out += v.split()
    return out


def check_units(root, problems):
    udir = os.path.join(root, 'units')
    for name in REQUIRED_UNITS:
        p = os.path.join(udir, name)
        if not os.path.isfile(p):
            problems.append(f'missing unit: units/{name}')
    if problems:
        return
    m = parse_unit(os.path.join(udir, 'khaen-ptp-master.service'))
    p = parse_unit(os.path.join(udir, 'khaen-ptp-phc2sys.service'))
    t = parse_unit(os.path.join(udir, 'khaen-ptp.target'))
    ex = words(m, 'Service', 'ExecStart')
    if ex != ['/usr/local/lib/khaen/ptp-master-start']:
        problems.append(f'master ExecStart must be /usr/local/lib/khaen/ptp-master-start (got {ex})')
    ex = words(p, 'Service', 'ExecStart')
    if ex != ['/usr/local/lib/khaen/ptp-phc2sys-start']:
        problems.append(f'phc2sys ExecStart must be /usr/local/lib/khaen/ptp-phc2sys-start (got {ex})')
    for label, u in (('master', m), ('phc2sys', p)):
        if '/etc/khaen-ptp/ptp-master.env' not in ' '.join(get(u, 'Service', 'EnvironmentFile')):
            problems.append(f'{label}: EnvironmentFile must load /etc/khaen-ptp/ptp-master.env')
        if not get(u, 'Service', 'RuntimeDirectory'):
            problems.append(f'{label}: RuntimeDirectory missing (/run/khaen-ptp for the UDS)')
    if 'khaen-ptp-phc2sys.service' not in words(m, 'Unit', 'Requires'):
        problems.append('master must Requires=khaen-ptp-phc2sys.service (PHC discipline before serving)')
    if 'khaen-ptp-phc2sys.service' not in words(m, 'Unit', 'After'):
        problems.append('master must be After=khaen-ptp-phc2sys.service')
    conflicts = set(words(m, 'Unit', 'Conflicts'))
    for legacy in ('khaen-ptp4l-master.service', 'khaen-phc2sys.service',
                   'khaen-statime-inferno.service', 'khaen-aes67-daemon.service'):
        if legacy not in conflicts:
            problems.append(f'master Conflicts must include legacy/slave-only PTP owner {legacy}')
    wants = set(words(t, 'Unit', 'Wants'))
    for need in ('khaen-ptp-master.service', 'khaen-ptp-phc2sys.service'):
        if need not in wants:
            problems.append(f'target Wants must include {need}')
    # capabilities: HW timestamping needs NET_ADMIN/RAW; UDP PTP ports 319/320 need NET_BIND_SERVICE.
    for label, u in (('master', m), ('phc2sys', p)):
        caps = ' '.join(get(u, 'Service', 'CapabilityBoundingSet'))
        for cap in ('CAP_NET_RAW', 'CAP_NET_ADMIN'):
            if cap not in caps:
                problems.append(f'{label}: CapabilityBoundingSet missing {cap}')
    if 'CAP_NET_BIND_SERVICE' not in ' '.join(get(m, 'Service', 'CapabilityBoundingSet')):
        problems.append('master: CapabilityBoundingSet missing CAP_NET_BIND_SERVICE (UDP 319/320)')
    if 'CAP_SYS_TIME' not in ' '.join(get(p, 'Service', 'CapabilityBoundingSet')):
        problems.append('phc2sys: CapabilityBoundingSet missing CAP_SYS_TIME (clock_adjtime PHC)')


def check_env(root, problems):
    path = os.path.join(root, 'etc', 'ptp-master.env.example')
    if not os.path.isfile(path):
        return
    text, raw = read_text(path)
    if b'\r' in raw:
        problems.append('env example contains CRLF')
    env = {}
    for i, line in enumerate(text.splitlines(), start=1):
        s = line.strip()
        if not s or s.startswith('#') or s.startswith(';'):
            continue
        if '=' not in s:
            problems.append(f'env line {i} is not KEY=VALUE: {s[:80]}')
            continue
        k, v = s.split('=', 1)
        k, v = k.strip(), v.strip()
        if k not in ENV_KEYS:
            problems.append(f'env line {i}: unsupported key {k}')
        env[k] = v
    if env.get('KHAEN_PTP_HW_TS') not in ('auto', 'on', 'off'):
        problems.append(f'KHAEN_PTP_HW_TS must be auto|on|off (got {env.get("KHAEN_PTP_HW_TS")!r})')
    try:
        off = int(env.get('KHAEN_PTP_UTC_OFFSET', ''))
    except ValueError:
        off = None
    if off is None or not (0 <= off <= 255):
        problems.append(f'KHAEN_PTP_UTC_OFFSET must be an integer 0..255 (got {env.get("KHAEN_PTP_UTC_OFFSET")!r})')


def check_conf(path, expect_transport, problems):
    name = os.path.basename(path)
    text, raw = read_text(path)
    if b'\r' in raw:
        problems.append(f'{name}: CRLF line endings')
    values = {}
    section = None
    for i, line in enumerate(text.splitlines(), start=1):
        s = line.strip()
        if not s or s.startswith('#') or s.startswith(';'):
            continue
        if s.startswith('[') and s.endswith(']'):
            section = s[1:-1]
            continue
        if '#' in s:
            problems.append(f'{name}:{i}: inline comment breaks the linuxptp parser: {s[:80]}')
            continue
        # linuxptp config is "key<whitespace>value"; accept key=value for the check as well.
        parts = s.split(None, 1)
        if len(parts) == 2:
            k, v = parts
        elif '=' in parts[0]:
            k, v = parts[0].split('=', 1)
        else:
            problems.append(f'{name}:{i}: not a key/value line: {s[:80]}')
            continue
        if '=' in k:
            k, v = (k.split('=', 1)[-1], v)
        values[k.strip()] = v.strip()
    if section != 'global' and 'global' not in [x.strip() for x in re.findall(r'^\[(.*)\]', text, re.M)]:
        problems.append(f'{name}: missing [global] section')
    for k, want in CONF_EXPECT.items():
        got = values.get(k)
        if got != want:
            problems.append(f'{name}: {k} should be {want!r}, got {got!r}')
    transport = values.get('network_transport')
    if transport != expect_transport:
        problems.append(f'{name}: network_transport should be {expect_transport}, got {transport!r}')
    for old in ('slaveOnly', 'masterOnly'):
        if old in values:
            problems.append(f'{name}: deprecated key {old} (linuxptp 4.2: use clientOnly/serverOnly)')
    uds = values.get('uds_address', '')
    if not uds.startswith('/run/khaen-ptp/'):
        problems.append(f'{name}: uds_address should live under /run/khaen-ptp, got {uds!r}')


def check_lib(root, problems):
    ldir = os.path.join(root, 'lib')
    for name in REQUIRED_LIB:
        path = os.path.join(ldir, name)
        if not os.path.isfile(path):
            problems.append(f'missing lib file: lib/{name}')
            continue
        text, raw = read_text(path)
        if b'\r' in raw:
            problems.append(f'lib/{name}: CRLF line endings')
        if not text.startswith('#!/bin/sh'):
            problems.append(f'lib/{name}: must start with #!/bin/sh')
    for name in ('ptp-master-start', 'ptp-phc2sys-start'):
        path = os.path.join(ldir, name)
        if os.path.isfile(path):
            text = open(path, encoding='utf-8', errors='replace').read()
            if '/opt/khaen-rt-stack/ptp/current/bin/' not in text:
                problems.append(f'lib/{name}: does not reference /opt/khaen-rt-stack/ptp/current/bin/')
    ml = os.path.join(ldir, 'ptp-master-start')
    if os.path.isfile(ml):
        text = open(ml, encoding='utf-8', errors='replace').read()
        for token in ('detect_ptp_phc', 'KHAEN_PTP_HW_TS', '-H', '-S'):
            if token not in text:
                problems.append(f'lib/ptp-master-start: missing fallback logic token {token!r}')


def main(argv):
    if len(argv) < 2 or argv[1].startswith('-'):
        print(json.dumps({'ok': False, 'error': 'usage: check_ptp_bundle.py <dir> [--json OUT]'}))
        return 2
    root = argv[1]
    out = None
    if '--json' in argv:
        out = argv[argv.index('--json') + 1]
    if not os.path.isdir(root):
        print(json.dumps({'ok': False, 'error': f'not a directory: {root}'}))
        return 2
    problems = []
    for sub in ('units', 'etc', 'lib'):
        if not os.path.isdir(os.path.join(root, sub)):
            problems.append(f'missing directory: {sub}/')
    check_units(root, problems)
    check_env(root, problems)
    check_lib(root, problems)
    check_conf(os.path.join(root, 'etc', 'ptp4l-master-udp.conf.example'), 'UDPv4', problems)
    check_conf(os.path.join(root, 'etc', 'ptp4l-master-l2.conf.example'), 'L2', problems)
    report = {'ok': not problems, 'root': os.path.abspath(root), 'problems': problems}
    text = json.dumps(report, ensure_ascii=False, indent=1)
    print(text)
    if out:
        with open(out, 'w', encoding='utf-8') as f:
            f.write(text + '\n')
    return 0 if not problems else 3


if __name__ == '__main__':
    sys.exit(main(sys.argv))
