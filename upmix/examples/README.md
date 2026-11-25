# 示例与测试证据

## examples/（生成产物，随生成器版本更新）

```bash
python3 tspi-upmix --layout-presets /etc/tspi-audio/layout-presets.yml \
  all -o examples --playback-channels 22
```

（无 layout-presets 时生成器拒绝用内置映射；仅本机临时调试可加 `--allow-builtin`。）

- `profiles/upmix-*.yml`：12 个 channelmap v2 profile（6 预设 × 完整 / *-safe）。
- `camilladsp/upmix-*.yml`：6 个独立 CamillaDSP ALSA 配置（playback 22ch，MOTU 用）；
  其他声卡用 `--playback-channels` 重新生成。
- `manifest.json`：文件清单与 sha256（校验部署一致性用）。

## reports/（2026-11-15 测试证据，**旧**：高度带限版本）

> 2026-11-16 用户裁决删除高度 1.8–10 kHz 带限与 18 ms 延迟后，profiles/camilladsp 已重新生成；
> 本目录 reports 仍是旧版历史证据（高度带限），最新本机证据见 `scratch/lead/doc-audit/run-b1a/report.md`
> （2026-11-16 全频高度复跑，156/156 entry、480 项检查 0 失败）。

- `local-report.md`：开发机 6 预设 × 13 信号 × 2 路径分析报告（480 项检查）。
- `board-report.md`：板端文件后端抽检分析报告（64 项检查，含板端计时）。
- `summary.json`：本机检查计数。
- `board-results.json`：板端计时原始数据（RK3576，各 chunksize 的 wall/CPU/RTF）。

这些报告由 `tests/run_file_backend.py` + `tests/analyze.py` 自动生成，可按
docs/TESTING.md 复现；报告中的路径为生成时路径。


## container/（H16 容器输入，task-19）

```bash
python3 tspi-upmix --layout-presets /etc/tspi-audio/layout-presets.yml \
  container -o examples/container --input-layout h16-7.1.4 --playback-channels 22
```

- profiles/`upmix-*-from-h16-7.1.4.yml`：容器输入（FL/FR 拿来上混）的 channelmap profile；
  其内部 `profile.name` 带 `-h16` 后缀（如 `upmix-4.1.2-music-h16`），避免与 2ch 同名冲突。
- profiles/`passthrough-h16-7.1.4-to-*.yml`：**跨布局下混**（C/BL/BR/TRL/TRR 折叠到 4.1.2/5.3.2，
  不做低音管理），不是同布局直通；供手动强制直通/WebUI 覆盖用。
- camilladsp/`autodetect-*.yml`：双路径单配置（Aux1=上混、Aux2=直通；daemon 用 fader 切换）。
- manifest.json：文件清单与 sha256。

说明见 docs/INPUT-MODE.md。

