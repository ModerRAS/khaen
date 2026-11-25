# 2.0 立体声上混（4.1.2 / 5.3.2）—— khaen-upmix

> 泰山派 3M（RK3576）/ Debian12 / 6.1.99-rt36 · CamillaDSP 4.1.3 · channelmap v2
> 任务：2.0（2ch）上混到用户当前/常用布局 **4.1.2** 与 **5.3.2**；不做 16ch 扩展、不做跨布局 remap。

## 1. 交付内容

```
projects/khaen-audio-dsp/upmix/
├── khaen-upmix                 # 生成器（Python3 + PyYAML；板端可直接运行）
├── presets/upmix-presets.yml  # 预设参数（音乐/电影/宽场 × 4.1.2/5.3.2）
├── examples/                  # 生成的示例（12 个 channelmap profile + 6 个 CamillaDSP 配置 + manifest；
│                              #  container/ 另有 8 个 H16 容器 profile + 6 个双路径配置）
├── deploy/install-upmix.sh    # 板端热部署：备份 + 安装 + 校验 + 自动回滚（不启动/不重启服务）
├── tests/                     # 信号生成/文件后端运行/分析/板端计时
├── patches/                   # 发现并由 audio-stack 合入的 khaen-channelmap 修复（存档）
├── docs/                      # 算法、WebUI 对接、测试方法
└── REPORT.md                  # 简短报告（含测试数据）
```

两种启用路径：

| 路径 | 依赖 | 适合 |
| --- | --- | --- |
| **A. channelmap profile + WebUI** | 修复后的 khaen-channelmap（OEM v5 已含） | 日常使用：WebUI 里选预设 → 加载 → 应用；参数可用现有角色校准 UI 微调 |
| **B. 独立 CamillaDSP 配置** | 只要 camilladsp | 不依赖 channelmap 渲染器；SSH 热部署 / 文件后端测试 / 直连 CamillaDSP |

## 2. 快速开始（开发机）

```bash
# 1) 列出/查看预设
python3 khaen-upmix list
python3 khaen-upmix show upmix-4.1.2-music

# 2) 自检（含真实 khaen-channelmap 校验，若 PATH 里能找到）
python3 khaen-upmix check upmix-4.1.2-music

# 3) 生成全部示例（必须给 layout-presets；缺省时生成器拒绝用内置映射，
#    仅本机临时调试可加 --allow-builtin）
python3 khaen-upmix --layout-presets /etc/khaen-audio/layout-presets.yml \
  all -o examples --playback-channels 22

# 4) 文件后端测试（本机 CamillaDSP 4.1.3）
python3 tests/gen_signals.py --out /tmp/upmix-signals
python3 tests/run_file_backend.py --workdir /tmp/upmix-run \
  --signals /tmp/upmix-signals --camilladsp /path/to/camilladsp \
  --khaen-upmix ./khaen-upmix \
  --layout-presets /etc/khaen-audio/layout-presets.yml \
  --channelmap /usr/local/bin/khaen-channelmap \
  --variants standalone,renderer \
  --presets upmix-4.1.2-music,upmix-5.3.2-music
python3 tests/analyze.py --workdir /tmp/upmix-run --report /tmp/upmix-run/report.md
```

## 3. 板端热部署（不重烧镜像）

```bash
# 开发机：生成 dist（见 deploy/README.md 的目录结构）并 scp 到板子
python3 khaen-upmix --layout-presets /etc/khaen-audio/layout-presets.yml \
  all -o /tmp/upmix-examples
# 板端：
cd /tmp/upmix-dist && bash install-upmix.sh --dry-run   # 先看计划
cd /tmp/upmix-dist && bash install-upmix.sh             # 安装+校验（失败自动回滚）
bash /tmp/upmix-dist/install-upmix.sh --rollback        # 回滚安装
```

安装内容（**只写文件，不 systemctl start/restart，不打开声卡**）：

- /usr/local/bin/khaen-upmix
- /etc/khaen-audio/upmix/presets.yml + camilladsp-examples/
- /etc/khaen-audio/profiles/upmix-*.yml（12 个：6 预设 × 完整/safe）
- /etc/khaen-audio/profiles/*-from-h16-*.yml（8 个容器输入 profile）+ passthrough-*.yml（2 个跨布局下混）
- 首次安装时快照当前 active 为 upmix-off-restore.yml（用于关闭上混）

## 4. 启用/关闭上混

### A. WebUI（推荐）

1. 打开「声道分配 / 校准」页，profile 列表会看到 `upmix-4.1.2-music` 等（安装后刷新）。
2. 选一个 → **加载**（写入 active，旧 active 自动备份）→ **校验** → **应用**。
   - 模式 off 时只渲染/校验，不启动 CamillaDSP；模式 dante/aes67 时会重启 CamillaDSP。
3. 关闭：加载 `upmix-off-restore`（首次安装时的 active 快照）；若没有该文件，加载用户自己的 profile。
4. 微调：加载后可在现有 UI 里改每角色增益/延迟/极性/高通/低通/PEQ（上混参数与这些字段一一对应，见
   docs/WEBUI-INTEGRATION.md）。

> 注意：`*-safe` 变体只去掉 `sources[].inverted`（源反相）依赖，**不能绕过旧渲染器 bug A**——bug A 影响所有带 per-role 分频/PEQ/FIR/gain/delay 的 profile（Filter step 的 channels/names 配对错）。旧镜像（<OEM v5）唯一可用路径 = 独立 CamillaDSP 配置（`khaen-upmix apply`）或先打 `patches/` 补丁；旧渲染器上 FL 1 kHz 约 −42 dB 是 bug A 特征（C-04）。

### B. 独立配置（SSH / 无 WebUI）

```bash
khaen-upmix apply upmix-4.1.2-music --dry-run     # 预览：显示 capture/playback/设备与配置头
khaen-upmix apply upmix-4.1.2-music               # 模式 off：只渲染+校验+写 /run/khaen/camilladsp.yml
khaen-upmix apply upmix-5.3.2-movie --mode dante  # 96k/Dante（inferno capture）
khaen-upmix rollback                              # 回滚最近一次 apply（备份校验，失败恢复）
```

`apply` 的默认目标是 `/run/khaen/camilladsp.yml`（与 khaen-mode/channelmap 同一运行期路径），备份在
`/var/lib/khaen/upmix/backups/`，state 在 `/var/lib/khaen/upmix/state.json`。模式非 off 且未传
`--no-restart` 时会重启 `khaen-camilladsp.service`；失败自动恢复备份。

## 5. 预设

| preset | 布局 | 说明 |
| --- | --- | --- |
| upmix-4.1.2-music | 4.1.2 | 环绕 -6dB/延迟 15ms；高度 -9dB 全频（无带限/延迟），保留立体声结像 |
| upmix-4.1.2-movie | 4.1.2 | 环绕 -3dB + 少量同相填充，高度 -6dB，包围感更强 |
| upmix-4.1.2-wide | 4.1.2 | 左右延迟差 3ms（高度无人工延迟）+ 中置填充，扩散感 |
| upmix-5.3.2-music | 5.3.2 | 中置 -6dB（200Hz-12kHz 带限）、环绕 -6dB、高度 -9dB 全频（无带限/延迟） |
| upmix-5.3.2-movie | 5.3.2 | 中置 -3dB、环绕 -3dB、高度 -6dB |
| upmix-5.3.2-wide | 5.3.2 | 中置 -4.5dB、左右延迟差，适合现场/氛围素材 |

每个预设另有 `*-safe` 变体（环绕/高度仅用 mid 合成，无源反相）。

> 表中高度 "−9/−6 dB" 是**角色增益**；高度源 mid 每源 −6.02 dB，故相对前置总电平约 −15 dB（music）/ −12 dB（movie）。

## 6. 通道映射

- 4.1.2（MOTU 48k 22ch 映射）：FL=0、FR=1、SL=2、SR=3、HFL=4、HFR=5、**SUB1=7**（ch6 空；8 个逻辑槽位，设备仍 22ch）。
  ch6 保留/SUB=7 的来源是板端部署表（task-19 裁决），文档不另做推导；运行时必须解析 layout-presets 并校验设备宽度（A-P1-16）。
- 5.3.2：FL=0、FR=1、C=2、SL=3、SR=4、SUB1=5、SUB2=6、SUB3=7、HFL=8、HFR=9。
- 以上来自 `/etc/khaen-audio/layout-presets.yml`（audio-stack 维护）；生成器读取该文件，**唯一真相**。
  内置表仅用于 `check` 对照/极端兜底：`all`/`install` 在缺 layout-presets 时直接报错退出
  （除非显式 `--allow-builtin`），避免用错的内置映射生成配置；`check` 会逐项断言内置表与
  layout-presets 一致（4.1.2 内置已按板端修正为 FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7）。

## 7. 文件/数据位置

| 路径 | 内容 |
| --- | --- |
| /etc/khaen-audio/profiles/upmix-*.yml | channelmap v2 profile（WebUI 可见） |
| /etc/khaen-audio/upmix/presets.yml | 参数预设（再生成用） |
| /etc/khaen-audio/upmix/camilladsp-examples/ | 独立 CamillaDSP ALSA 示例 |
| /run/khaen/camilladsp.yml | apply 后的运行期配置（与现有约定一致） |
| /var/lib/khaen/upmix/ | apply 备份/状态、install 备份 |

## 8. 测试结果摘要

- 6 预设 × 13 测试信号 × 2 路径（独立配置 / channelmap 渲染）：**480 项检查 0 失败**；
  与参考 DSP 仿真逐样本一致（max diff ~1e-8），两条路径输出**逐样本相同**（diff = 0）。
  **该断言只证明渲染器忠实执行 plan，不证明 plan/映射本身正确**（A-P1-15）；plan 正确性另由映射断言/人工复核。
- 板端（OEM v5，RK3576 6.1.99-rt36）：28/28 文件后端运行成功，64 项检查 0 失败；
  20s 立体声粉噪上混 CPU 0.46–1.67s（≈单核 2–8%），文件后端 RTF 2–19×（chunksize 128–2048）。
  **该板端数据为 2026-11-15 旧版（bandlimited height）实测，未在 2026-11-16 改造后复测，标 SUPERSEDED**。
- 详见 REPORT.md 与 docs/TESTING.md。

## 9. 已知限制

1. H16 输入布局/通道顺序**未上板实测**；12ch 容器是 flow 配置结果/高置信推断，不是 H16 固有属性（A-P1-11）。本工具只做 2ch 输入（pc-2ch），不做 16ch。
2. 未做 MOTU 真机播放冒烟（按 Lead 安排由 audio-stack 执行；本工具测试全程不占声卡）。
3. 4.1.2 无中置，对白/中心内容留在 FL/FR（虚拟中置）。
4. 低音管理为 **FL/FR mid 简化路径**：FL/FR 高通 80Hz，SUB=LP((L+R)/2)；**C 80–200 Hz、SL/SR 80–100 Hz 未汇总、被丢弃**；`redirect_from` 仅校验用、未生效；真矩阵 BM/redirect 生效/能量守恒检查 deferred（C-20）。
5. allpass 去相关（Biquad 级联）只在独立 CamillaDSP 配置支持；channelmap profile 退化为固定延迟。
6. channelmap 校验会报"输出角色无路由/将静音"的良性警告（显式 sources 不计入 remap 统计），不影响输出。

## 10. 相关文档

- docs/ALGORITHM.md —— 信号设计、参数、梳状/相位说明（含仍存在的跨通道梳状）
- docs/WEBUI-INTEGRATION.md —— WebUI 零改动流程、参数映射、可选专用卡片契约
- docs/TESTING.md —— 本地/板端测试步骤与判定标准
- docs/PEQ-TYPE.md —— peq[].type（peaking/low_shelf/high_shelf）语义、shelf slope=12q 映射与 CamillaDSP 边界
- deploy/README.md —— 热部署与回滚
- patches/ —— khaen-channelmap 三处修复（已由 audio-stack 合入）


## 11. 输入内容模式：7.1.4 容器里只有 2.0（task-19）

H16 默认 Pure，容器可能是 7.1.4（12ch，**未上板实测/高置信推断**，A-P1-11），节目可能只有 2.0；也可能 H16 已内部上混（Dolby Upmix /
DTS Neural:X）。板端要自动判断：只有 FL/FR 有能量 → 上混；C/SL/SR/height 有能量 → 直通（防双上混）。
完整契约/判定规则/部署见 **docs/INPUT-MODE.md**。

```bash
# 双路径配置（一个 CamillaDSP 配置里同时有上混/直通，daemon 用 Aux1/Aux2 切换，无重启）
khaen-upmix autodetect-config upmix-4.1.2-music -o /tmp/auto.yml \
  --input-layout h16-7.1.4 --content-mode auto      # stereo|multichannel 可固定单路径

# 离线检测容器 WAV（H16 12ch）；输出与运行态同字段 JSON
khaen-upmix detect --wav /tmp/container714.wav --input-layout h16-7.1.4

# 手动覆盖：auto / 强制 2.0 / 强制多声道（force-* 别名也接受）
khaen-upmix input-mode set stereo
khaen-upmix input-mode get

# daemon：读 CamillaDSP websocket 的 GetCaptureSignalRms → 写 /run/khaen/input-content.json + 切 fader
khaen-upmix autodetect --input-layout h16-7.1.4
```

- 运行态：`/run/khaen/input-content.json`（schema `khaen-input-content/v1`，WebUI 只读）。
- 覆盖持久化：`/etc/khaen-audio/input-mode.json`（mode: auto|stereo|multichannel）。
- 判断只用 C/SL/SR/height，**LFE 不作判据**；窗口 2.5s + 迟滞 6dB + 启动冻结 2.5s；auto 默认直通。
- 安装包含 `khaen-upmix-autodetect.service.example`（默认不启用，不启停音频服务）。
- 容器 profile / autodetect 配置示例：`examples/container/`。

