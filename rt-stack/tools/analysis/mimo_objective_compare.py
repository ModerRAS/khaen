#!/usr/bin/env python3
"""Compare v1 (emodal) runs with round-2 objective variants (SIMULATED / UNPROVEN).

Reads report.json files only (no recomputation): intended for building the evidence
comparison table; independent numerical verification of the same numbers lives in
mimo_roomforge_metrics.py (Python reference metrics).
"""
from __future__ import annotations

import argparse, csv, json, os, sys

VARIANT_IDS = ['B0', 'B1', 'B2', 'B3']

def load_reports(root):
    """root/<scene>/report.json -> {scene_id: report}"""
    out = {}
    if not root or not os.path.isdir(root):
        return out
    for name in sorted(os.listdir(root)):
        p = os.path.join(root, name, 'report.json')
        if os.path.isfile(p):
            try:
                d = json.load(open(p, encoding='utf-8'))
            except Exception as exc:
                print(f'WARNING: cannot read {p}: {exc}', file=sys.stderr)
                continue
            out[d.get('scene_id') or name] = d
    return out

def load_obj_root(root):
    """root/<tag>/<scene>/report.json -> {tag: {scene_id: report}}"""
    out = {}
    if not root or not os.path.isdir(root):
        return out
    for tag in sorted(os.listdir(root)):
        tag_dir = os.path.join(root, tag)
        if os.path.isdir(tag_dir):
            out[tag] = load_reports(tag_dir)
    return out

def mean(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return sum(xs) / len(xs) if xs else None

def legacy_spatial_metrics(variant):
    """Old v1 reports only carry spatial[]; derive the round-2 metrics where possible."""
    res = {}
    for scope in ('design', 'holdout'):
        block = variant.get(scope) or {}
        spatial = block.get('spatial') or []
        stds = [s.get('std_db') for s in spatial if isinstance(s.get('std_db'), (int, float))]
        nulls = [s.get('interference_null_db') for s in spatial if isinstance(s.get('interference_null_db'), (int, float))]
        res[scope] = {
            'seat_std_mean_db': mean(stds),
            'seat_std_max_db': max(stds) if stds else None,
            'max_dev_db': None,
            'null_depth_mean_db': mean(nulls),
            'null_depth_worst_db': min(nulls) if nulls else None,
        }
    return res

def objective_metrics(variant, scope):
    block = variant.get(scope) or {}
    legacy = legacy_spatial_metrics(variant)[scope]
    def pick(key):
        v = block.get(key)
        return v if isinstance(v, (int, float)) else legacy.get(key)
    return {k: pick(k) for k in ('seat_std_mean_db', 'seat_std_max_db', 'max_dev_db',
                                 'null_depth_mean_db', 'null_depth_worst_db')}

def variant_row(scene_id, tag, report, vid):
    variants = report.get('variants') or {}
    v = variants.get(vid) if isinstance(variants, dict) else None
    if v is None:
        return None
    de = v.get('design') or {}
    ho = v.get('holdout') or {}
    cons = v.get('constraints') or {}
    feas = v.get('feasibility') or {}
    dm = objective_metrics(v, 'design')
    hm = objective_metrics(v, 'holdout')
    return {
        'scene_id': scene_id,
        'tag': tag,
        'variant': vid,
        'objective': (v.get('objective') or {}).get('name'),
        'design_e_modal_db': de.get('e_modal_db'),
        'design_r_max': de.get('r_max'),
        'design_energy_migration': de.get('energy_migration'),
        'design_seat_std_mean_db': dm['seat_std_mean_db'],
        'design_seat_std_max_db': dm['seat_std_max_db'],
        'design_max_dev_db': dm['max_dev_db'],
        'design_null_depth_mean_db': dm['null_depth_mean_db'],
        'design_null_depth_worst_db': dm['null_depth_worst_db'],
        'holdout_e_modal_db': ho.get('e_modal_db'),
        'holdout_consistency_c': ho.get('consistency_c'),
        'holdout_severe_regression': ho.get('severe_regression'),
        'holdout_seat_std_mean_db': hm['seat_std_mean_db'],
        'holdout_max_dev_db': hm['max_dev_db'],
        'holdout_null_depth_mean_db': hm['null_depth_mean_db'],
        'feasible': feas.get('feasible'),
        'design_max_rise_db': feas.get('design_max_rise_db'),
        'predicted_status': v.get('predicted_status'),
        'global_scale': cons.get('global_scale'),
    }

def fmt(x, spec='{:.2f}'):
    if x is None:
        return '—'
    if isinstance(x, bool):
        return 'YES' if x else 'no'
    try:
        return spec.format(x)
    except Exception:
        return str(x)

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--runs', required=True, help='v1 runs dir (tag: v1-emodal)')
    ap.add_argument('--runs-obj', required=True, help='round-2 root: <tag>/<scene>/report.json')
    ap.add_argument('--out', required=True, help='validation output dir')
    args = ap.parse_args()
    old = load_reports(args.runs)
    new = load_obj_root(args.runs_obj)
    scenes = sorted(set(old) | {s for reports in new.values() for s in reports})
    rows = []
    for scene_id in scenes:
        if scene_id in old:
            for vid in VARIANT_IDS:
                row = variant_row(scene_id, 'v1-emodal', old[scene_id], vid)
                if row: rows.append(row)
        for tag in sorted(new):
            if scene_id in new[tag]:
                for vid in VARIANT_IDS:
                    row = variant_row(scene_id, tag, new[tag][scene_id], vid)
                    if row: rows.append(row)
    os.makedirs(args.out, exist_ok=True)
    csv_path = os.path.join(args.out, 'objective-comparison.csv')
    fields = ['scene_id', 'tag', 'variant', 'objective', 'design_e_modal_db', 'design_r_max',
              'design_energy_migration', 'design_seat_std_mean_db', 'design_seat_std_max_db',
              'design_max_dev_db', 'design_null_depth_mean_db', 'design_null_depth_worst_db',
              'holdout_e_modal_db', 'holdout_consistency_c', 'holdout_severe_regression',
              'holdout_seat_std_mean_db', 'holdout_max_dev_db', 'holdout_null_depth_mean_db',
              'feasible', 'design_max_rise_db', 'predicted_status', 'global_scale']
    with open(csv_path, 'w', encoding='utf-8', newline='') as fh:
        w = csv.DictWriter(fh, fieldnames=fields, lineterminator='\n')
        w.writeheader()
        for row in rows:
            w.writerow(row)
    summary = {
        'format': 'khaen-mimo-objective-comparison/v1',
        'source': 'SIMULATED',
        'honesty_status': 'UNPROVEN',
        'tags': ['v1-emodal'] + sorted(new),
        'rows': rows,
    }
    with open(os.path.join(args.out, 'objective-comparison.json'), 'w', encoding='utf-8') as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=2, sort_keys=True)
    # markdown tables (B2/B3 only: they are the MIMO variants) for the REPORT
    print('## Round-2 objective comparison — design (B2/B3)')
    print()
    print('| scene | tag | variant | e_modal dB | seat std mean dB | max dev dB | null depth mean dB | R_max | feasible | status |')
    print('| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |')
    for row in rows:
        if row['variant'] not in ('B2', 'B3'):
            continue
        print(f"| {row['scene_id']} | {row['tag']} | {row['variant']} | {fmt(row['design_e_modal_db'])} | {fmt(row['design_seat_std_mean_db'])} | {fmt(row['design_max_dev_db'])} | {fmt(row['design_null_depth_mean_db'])} | {fmt(row['design_r_max'], '{:.3f}')} | {fmt(row['feasible'])} | {row['predicted_status']}")
    print()
    print('## Round-2 objective comparison — holdout (B2/B3)')
    print()
    print('| scene | tag | variant | e_modal dB | seat std mean dB | max dev dB | null depth mean dB | C | severe | status |')
    print('| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |')
    for row in rows:
        if row['variant'] not in ('B2', 'B3'):
            continue
        print(f"| {row['scene_id']} | {row['tag']} | {row['variant']} | {fmt(row['holdout_e_modal_db'])} | {fmt(row['holdout_seat_std_mean_db'])} | {fmt(row['holdout_max_dev_db'])} | {fmt(row['holdout_null_depth_mean_db'])} | {fmt(row['holdout_consistency_c'])} | {fmt(row['holdout_severe_regression'])} | {row['predicted_status']}")
    print()
    print(f'wrote {csv_path} and objective-comparison.json; rows={len(rows)}')

if __name__ == '__main__':
    main()