#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""tspi-channelmap peq[].type 回归测试（离线；不连板、不启服务）。

覆盖（Lead 裁决 2026-11-16，calibration-spec/review/V11-CONSUMER-DECISIONS-2026-11-16.md）：
  1. peaking 缺省/显式 type：渲染仍为 CamillaDSP Peaking{freq,q,gain}，不产生 type/slope 字段；
  2. low_shelf/high_shelf：渲染为 Lowshelf/Highshelf{freq,slope=12*q,gain}，不产生 q 字段；
  3. 未知 type：rc=2（schema），不产出文件；
  4. peaking 的 q 越界 [0.1,10]：rc=3，不产出文件；
  5. shelf 的 q 越界 [0.1,1.0]（含 1.01 与 2.0）：rc=3，不产出文件（裁决 5；不再只 warning）；
  6. shelf q=1.0（slope=12.0）边界可渲染；
  7. 既有 peaking-only 渲染与改动前逐字节一致（本机用 git HEAD 版本对照，见 REPORT）；
     本测试退化为结构断言，可在板端/任意 checkout 运行。

用法：
    python3 upmix/tests/test_channelmap_peq_type.py -v
依赖：python3-yaml；camilladsp 可选（存在时 render 会跑 --check）。
"""
import copy
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
CM = ROOT / "audio" / "overlay" / "usr" / "local" / "bin" / "tspi-channelmap"
PRESETS = ROOT / "audio" / "overlay" / "etc" / "tspi-audio" / "layout-presets.yml"
BASE = ROOT / "upmix" / "examples" / "container" / "profiles" / "upmix-4.1.2-movie-from-h16-7.1.4.yml"

CAP_CH = "12"
PB_CH = "8"


class ChannelmapPeqTypeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for p in (CM, PRESETS, BASE):
            if not p.is_file():
                raise unittest.SkipTest("缺少测试依赖：%s" % p)
        cls.tmp = tempfile.mkdtemp(prefix="cm-peqtype-")
        cls.env = dict(os.environ)
        cls.env.update({
            "TSPI_CHANNELMAP_RUN_DIR": os.path.join(cls.tmp, "run"),
            "TSPI_CHANNELMAP_VAR_DIR": os.path.join(cls.tmp, "var"),
            "PYTHONIOENCODING": "utf-8",
            "PYTHONUTF8": "1",
        })
        cls.base = yaml.safe_load(BASE.read_text(encoding="utf-8"))
        cls.camilladsp = shutil.which("camilladsp")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    # ---------- helpers ----------
    def profile(self, name, peq):
        d = copy.deepcopy(self.base)
        role = next(r for r in d["output"]["roles"] if r["id"] == "FL")
        role["peq"] = peq
        path = os.path.join(self.tmp, name + ".yml")
        with open(path, "w", encoding="utf-8") as f:
            yaml.safe_dump(d, f, sort_keys=False, allow_unicode=True)
        return path

    def run_cm(self, *args):
        return subprocess.run(
            [sys.executable, str(CM)] + list(args) + ["--presets", str(PRESETS)],
            capture_output=True, text=True, encoding="utf-8", errors="replace",
            env=self.env, cwd=self.tmp)

    def render(self, profile, out=None, check=True):
        args = ["render", profile, "--capture-channels", CAP_CH,
                "--playback-channels", PB_CH, "--playback-device", "hw:motu,0"]
        if not check:
            args.append("--no-check")
        args += ["-o", out if out else "-"]
        return self.run_cm(*args)

    def filters_of(self, rendered_text):
        return yaml.safe_load(rendered_text)["filters"]

    # ---------- tests ----------
    def test_peaking_default_type(self):
        p = self.profile("peaking_default", [{"freq": 100.0, "q": 0.7, "gain_db": -3.0}])
        r = self.render(p)
        self.assertEqual(r.returncode, 0, r.stderr)
        f = self.filters_of(r.stdout)["fl_peq0"]["parameters"]
        self.assertEqual(f, {"type": "Peaking", "freq": 100.0, "q": 0.7, "gain": -3.0})

    def test_peaking_explicit_type_is_normalized_away(self):
        p = self.profile("peaking_explicit",
                         [{"type": "peaking", "freq": 100.0, "q": 0.7, "gain_db": -3.0}])
        r = self.render(p)
        self.assertEqual(r.returncode, 0, r.stderr)
        f = self.filters_of(r.stdout)["fl_peq0"]["parameters"]
        self.assertEqual(f, {"type": "Peaking", "freq": 100.0, "q": 0.7, "gain": -3.0})
        # 归一化后的 peq_out 对 peaking 省略 type（与 producer Peq serde 一致）
        rp = self.run_cm("print", p, "--json")
        self.assertEqual(rp.returncode, 0, rp.stderr)
        resolved = json_loads_ok(rp.stdout)
        role = resolved["resolved"]["output"]["roles"]["FL"]
        self.assertNotIn("type", role["peq"][0])

    def test_shelf_mapping(self):
        p = self.profile("shelf", [
            {"type": "low_shelf", "freq": 100.0, "q": 0.7, "gain_db": -3.0},
            {"type": "high_shelf", "freq": 5000.0, "q": 0.5, "gain_db": 2.0},
        ])
        r = self.render(p)
        self.assertEqual(r.returncode, 0, r.stderr)
        filters = self.filters_of(r.stdout)
        self.assertEqual(filters["fl_peq0"]["parameters"],
                         {"type": "Lowshelf", "freq": 100.0, "slope": 8.4, "gain": -3.0})
        self.assertEqual(filters["fl_peq1"]["parameters"],
                         {"type": "Highshelf", "freq": 5000.0, "slope": 6.0, "gain": 2.0})
        for name in ("fl_peq0", "fl_peq1"):
            self.assertNotIn("q", filters[name]["parameters"])

    def test_shelf_q_at_limit(self):
        p = self.profile("shelf_limit",
                         [{"type": "low_shelf", "freq": 100.0, "q": 1.0, "gain_db": -3.0}])
        r = self.render(p)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.filters_of(r.stdout)["fl_peq0"]["parameters"]["slope"], 12.0)

    def test_unknown_type_schema_error(self):
        p = self.profile("unknown", [{"type": "notch", "freq": 100.0, "q": 1.0, "gain_db": -3.0}])
        out = os.path.join(self.tmp, "unknown_out.yml")
        r = self.render(p, out=out)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertFalse(os.path.exists(out))
        self.assertIn("type", r.stderr)

    def test_q_out_of_range(self):
        for name, q in (("q_high", 11.0), ("q_low", 0.05)):
            p = self.profile(name, [{"freq": 100.0, "q": q, "gain_db": -3.0}])
            out = os.path.join(self.tmp, name + "_out.yml")
            r = self.render(p, out=out)
            self.assertEqual(r.returncode, 3, "%s: %s" % (name, r.stderr))
            self.assertFalse(os.path.exists(out))

    def test_shelf_q_above_1_rejected(self):
        # 裁决 5：shelf q=S>1 直接 rc=3，不渲染、不把 slope>12 留给 camilladsp --check。
        for name, q in (("shelf_q101", 1.01), ("shelf_q2", 2.0)):
            p = self.profile(name, [{"type": "high_shelf", "freq": 100.0, "q": q, "gain_db": -3.0}])
            out = os.path.join(self.tmp, name + "_out.yml")
            r = self.render(p, out=out)
            self.assertEqual(r.returncode, 3, "%s: %s" % (name, r.stderr))
            self.assertFalse(os.path.exists(out), name)
            self.assertIn("RBJ slope S", r.stderr)
            rv = self.run_cm("validate", p)
            self.assertEqual(rv.returncode, 3, "%s validate: %s" % (name, rv.stderr))

    def test_freq_and_gain_ranges(self):
        p = self.profile("freq_bad", [{"freq": 24000.0, "q": 1.0, "gain_db": 0.0}])
        self.assertEqual(self.render(p, out=os.path.join(self.tmp, "f1.yml")).returncode, 3)
        p = self.profile("gain_bad", [{"freq": 100.0, "q": 1.0, "gain_db": -30.0}])
        self.assertEqual(self.render(p, out=os.path.join(self.tmp, "f2.yml")).returncode, 3)


def json_loads_ok(text):
    import json
    return json.loads(text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
