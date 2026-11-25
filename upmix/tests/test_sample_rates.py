#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""F-01 回归：upmix 开发副本与板端 overlay 统一拒绝 176400/192000（稳定错误码）。

不依赖 pytest；直接 `python upmix/tests/test_sample_rates.py`，exit 0=PASS。
"""
import importlib.util
import os
import sys
from importlib.machinery import SourceFileLoader

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "..", "tspi-upmix")


def load_tool():
    loader = SourceFileLoader("tspi_upmix_tool", TOOL)
    spec = importlib.util.spec_from_loader("tspi_upmix_tool", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


def expect_die(mod, fs, code):
    try:
        mod.check_fs(fs)
    except SystemExit as exc:
        assert exc.code == 2, (fs, exc.code)
        return
    raise AssertionError("check_fs(%r) 未拒绝" % (fs,))


def main():
    mod = load_tool()
    # 白名单
    for fs in (44100, 48000, 88200, 96000):
        assert mod.check_fs(fs) == fs
    # 明确禁用：176400/192000（F-01）
    for fs in (176400, 192000):
        expect_die(mod, fs, 2)
    # 非白名单值/非数值：稳定错误码路径
    expect_die(mod, 32000, 2)
    expect_die(mod, "192k", 2)
    # build_plan 也必须拒绝（真实预设下；upmix/presets/upmix-presets.yml）
    presets = mod.load_presets()
    preset_ids = list(presets.get("presets", {}).keys()) if isinstance(presets, dict) else []
    assert preset_ids, "upmix/presets/upmix-presets.yml 无预设"
    for fs in (176400, 192000):
        try:
            mod.build_plan(preset_ids[0], presets, fs=fs)
        except SystemExit as exc:
            assert exc.code == 2
        else:
            raise AssertionError("build_plan(fs=%r) 未拒绝" % (fs,))
    # 白名单 build_plan 不因采样率被拒
    plan = mod.build_plan(preset_ids[0], presets, fs=48000)
    assert plan["fs"] == 48000
    print("PASS sample-rate whitelist: 44.1/48/88.2/96k ok; 176.4/192k rejected (F-01)")
    return 0


if __name__ == "__main__":
    sys.exit(main())