#!/usr/bin/env python3
"""TSPI IRQ 亲和检查（task-192；deploy 侧 T45 依赖 IRQ 放置前置的最小检查器）。

背景（task-189 实测，证据 evidence/board-irq-affinity/）：
  T45 scheduler plan() 要求 worker 依赖 IRQ（bit0/1 USB、bit2 PTP、bit3 eth）不落在
  DSP_CPUS={4,5,6,7}。板端把 IRQ58(xhci/MOTU)→CPU1、IRQ56(end0/PTP)→CPU2、IRQ57(end0 第二队列)
  →CPU3 后 plan accepted；但 irqbalance 重启 ≤25s 会搬回 A72。部署侧因此需要在 preflight/verify
  反复校验「依赖 IRQ effective 亲和仍在 A53」。

本脚本做什么：读取 /proc/irq/<irq>/{smp_affinity_list,effective_affinity_list} 与
/sys/devices/system/cpu/online，按配置的 pins 校验每个 IRQ 的 effective 亲和是目标单核，并检查
T45 依赖角色（usb/ptp/eth）不落在 DSP_CPUS。
本脚本不做什么（如实限制）：它不是运行 tspi_rt_host::scheduler::plan() 本体——只覆盖 plan 的
IRQ 放置维度；真实 plan() accepted 证据见 evidence/board-irq-affinity/t45-probe-*.json。
真实板端 /sys 的 allowed/isolated 语义比本检查更全；本检查缺文件时记 SKIPPED 而不是假装通过。

用法:
  irq_affinity.py check [--root DIR] [--pins "58:1 56:2 57:3"] [--dsp-cpus "4,5,6,7"]
                        [--t45-map "usb=58,ptp=56,eth=56"] [--json OUT]
  退出码: 0=全部 PASS；3=有 FAIL；2=用法错误。
"""
import datetime
import json
import os
import sys

SCHEMA = 'tspi-rt-stack/irq-affinity-check/v1'
DEFAULT_PINS = '58:1 56:2 57:3'
DEFAULT_DSP = '4,5,6,7'
DEFAULT_T45 = 'usb=58,ptp=56,eth=56'


def now_iso():
    return datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec='seconds')


def join(root, path):
    if root in (None, '', '/'):
        return path
    return root.rstrip('/\\') + path


def read_text(path):
    try:
        with open(path, encoding='utf-8', errors='replace') as f:
            return f.read().strip()
    except OSError:
        return None


def parse_cpu_list(s):
    cpus = set()
    if not s:
        return cpus
    for part in s.replace(';', ',').split(','):
        part = part.strip()
        if not part:
            continue
        if '-' in part:
            a, _, b = part.partition('-')
            try:
                for i in range(int(a), int(b) + 1):
                    cpus.add(i)
            except ValueError:
                continue
        else:
            try:
                cpus.add(int(part))
            except ValueError:
                continue
    return cpus


def parse_pins(s):
    pins = []
    for tok in (s or '').split():
        irq_s, sep, cpu_s = tok.partition(':')
        if not sep:
            raise ValueError('pin 缺少冒号: %r（要求 <irq>:<cpu>）' % tok)
        pins.append((int(irq_s), int(cpu_s)))
    if not pins:
        raise ValueError('pins 为空')
    return pins


def parse_map(s):
    m = {}
    for tok in (s or '').split(','):
        k, sep, v = tok.partition('=')
        if not sep:
            raise ValueError('t45-map 条目缺少等号: %r（要求 role=irq）' % tok)
        m[k.strip()] = int(v.strip())
    return m


def effective_of(root, irq):
    smp_path = join(root, '/proc/irq/%d/smp_affinity_list' % irq)
    eff_path = join(root, '/proc/irq/%d/effective_affinity_list' % irq)
    smp_raw = read_text(smp_path)
    if smp_raw is None:
        return None
    eff_raw = read_text(eff_path)
    return {
        'smp_affinity_list': smp_raw,
        'effective_affinity_list': eff_raw if eff_raw is not None else smp_raw,
        'effective_source': 'effective_affinity_list' if eff_raw is not None else 'smp_affinity_list(fallback)',
    }


def main(argv):
    root = '/'
    pins_s = DEFAULT_PINS
    dsp_s = DEFAULT_DSP
    t45_s = DEFAULT_T45
    out_json = None
    i = 1
    if i < len(argv) and argv[i] == 'check':
        i = 2
    elif i < len(argv) and argv[i] in ('-h', '--help'):
        print(__doc__)
        return 0
    while i < len(argv):
        a = argv[i]
        if a == '--root' and i + 1 < len(argv):
            root = argv[i + 1]; i += 2
        elif a == '--pins' and i + 1 < len(argv):
            pins_s = argv[i + 1]; i += 2
        elif a == '--dsp-cpus' and i + 1 < len(argv):
            dsp_s = argv[i + 1]; i += 2
        elif a == '--t45-map' and i + 1 < len(argv):
            t45_s = argv[i + 1]; i += 2
        elif a == '--json' and i + 1 < len(argv):
            out_json = argv[i + 1]; i += 2
        elif a in ('-h', '--help'):
            print(__doc__)
            return 0
        else:
            print(json.dumps({'ok': False, 'error': '未知参数: %s' % a}, ensure_ascii=False))
            return 2
    try:
        pins = parse_pins(pins_s)
        dsp = sorted(parse_cpu_list(dsp_s))
        t45_map = parse_map(t45_s)
    except ValueError as e:
        print(json.dumps({'ok': False, 'error': str(e)}, ensure_ascii=False))
        return 2

    online_raw = read_text(join(root, '/sys/devices/system/cpu/online'))
    online = parse_cpu_list(online_raw) if online_raw else None

    pin_entries = []
    for irq, want in pins:
        st = effective_of(root, irq)
        if st is None:
            entry = {'irq': irq, 'target_cpu': want, 'ok': False,
                     'error': '缺 /proc/irq/%d/smp_affinity_list（IRQ 不存在/未枚举）' % irq}
        else:
            smp_cpus = sorted(parse_cpu_list(st['smp_affinity_list']))
            eff_cpus = sorted(parse_cpu_list(st['effective_affinity_list']))
            pin_ok = (smp_cpus == [want]) and (eff_cpus == [want])
            on_dsp = bool(set(eff_cpus) & set(dsp))
            entry = {'irq': irq, 'target_cpu': want,
                     'smp_affinity_list': st['smp_affinity_list'],
                     'effective_affinity_list': st['effective_affinity_list'],
                     'effective_source': st['effective_source'],
                     'effective_cpus': eff_cpus, 'smp_cpus': smp_cpus,
                     'on_dsp_cpus': sorted(set(eff_cpus) & set(dsp)),
                     'ok': bool(pin_ok and not on_dsp)}
            if not pin_ok:
                entry['error'] = 'effective/smp 亲和不是目标单核'
            elif on_dsp:
                entry['error'] = '依赖 IRQ 落在 DSP_CPUS'
        pin_entries.append(entry)

    roles = []
    t45_fails = 0
    for role, irq in sorted(t45_map.items()):
        st = effective_of(root, irq)
        if st is None:
            roles.append({'role': role, 'irq': irq, 'ok': False, 'error': 'IRQ 不存在/未枚举'})
            t45_fails += 1
            continue
        eff_cpus = sorted(parse_cpu_list(st['effective_affinity_list']))
        on_dsp = sorted(set(eff_cpus) & set(dsp))
        if online is None:
            online_ok = 'SKIPPED'
        else:
            online_ok = all(c in online for c in eff_cpus)
        role_ok = (not on_dsp) and (online_ok is not False)
        roles.append({'role': role, 'irq': irq, 'effective_cpus': eff_cpus,
                      'on_dsp_cpus': on_dsp, 'online': online_ok,
                      'ok': bool(role_ok)})
        if not role_ok:
            t45_fails += 1

    pins_ok = all(p['ok'] for p in pin_entries)
    t45_ok = t45_fails == 0
    overall = pins_ok and t45_ok
    result = {
        'schema': SCHEMA,
        'checked_at': now_iso(),
        'root': root,
        'pins_config': pins_s,
        'pins': pin_entries,
        'pins_ok': pins_ok,
        'dsp_cpus': dsp,
        'online_cpus': sorted(online) if online is not None else 'SKIPPED(/sys/.../online 缺失)',
        't45': {
            'map': t45_map,
            'roles': roles,
            'irq_precondition_ok': t45_ok,
            'plan_equivalent': 'PASS' if t45_ok else 'FAIL',
            'engine': 'deploy 侧最小检查：只校验 plan 的 IRQ 放置维度，未调用 tspi_rt_host::scheduler::plan()',
        },
        'overall_ok': overall,
        'limitations': [
            '不是运行 plan() 本体；真实 plan() accepted 证据见 evidence/board-irq-affinity/t45-probe-*.json',
            '/sys CPU online 缺失时 online 校验记 SKIPPED（不假装通过）',
            '只覆盖 T45 依赖 IRQ（usb/ptp/eth）；A72 其余 IRQ 与 RT 抖动属 T29',
        ],
    }

    for p in pin_entries:
        if p.get('ok'):
            print('[PASS] IRQ%d -> CPU%d（smp=%s effective=%s）' % (
                p['irq'], p['target_cpu'], p.get('smp_affinity_list'), p.get('effective_affinity_list')))
        else:
            print('[FAIL] IRQ%d -> CPU%d（%s smp=%s effective=%s）' % (
                p['irq'], p['target_cpu'], p.get('error', 'FAIL'),
                p.get('smp_affinity_list'), p.get('effective_affinity_list')))
    for r in roles:
        if r.get('ok'):
            print('[PASS] T45 %s IRQ%d effective=%s（不在 DSP_CPUS=%s；online=%s）' % (
                r['role'], r['irq'], r['effective_cpus'], dsp, r['online']))
        else:
            print('[FAIL] T45 %s IRQ%d（%s effective=%s on_dsp=%s online=%s）' % (
                r['role'], r['irq'], r.get('error', 'FAIL'), r.get('effective_cpus'),
                r.get('on_dsp_cpus'), r.get('online')))
    print('RESULT %s（pins_ok=%s t45_irq_precondition_ok=%s）' % (
        'PASS' if overall else 'FAIL', pins_ok, t45_ok))

    if out_json:
        d = os.path.dirname(os.path.abspath(out_json))
        if d:
            os.makedirs(d, exist_ok=True)
        with open(out_json, 'w', encoding='utf-8') as f:
            f.write(json.dumps(result, ensure_ascii=False, indent=1) + '\n')
    return 0 if overall else 3


if __name__ == '__main__':
    sys.exit(main(sys.argv))
