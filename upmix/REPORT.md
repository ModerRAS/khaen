# 2.0 立体声上混交付报告（4.1.2 / 5.3.2）

日期：2026-11-15 · 环境：泰山派 3M（RK3576）/ Debian12 / kernel 6.1.99-rt36（OEM v5）/
CamillaDSP 4.1.3 · channelmap v2 · 任务 task-17

> **数据时效（B1a 2026-11-17）**：§3.1 本机数据已于 2026-11-16 高度全频化后复跑（480 项 0 失败，HFL/HFR 电平/延迟已更新）；
> §3.2 板端计时为 **2026-11-15 旧版（bandlimited height + 旧渲染器）**实测，未在改造后复测，标 **SUPERSEDED**，仅作历史参考。
> 文档/报告口径以本文件 + docs/ALGORITHM.md + TESTING.md 为准；旧 `examples/reports/` 已加旧版标签。

## 1. 结论

1. **2.0 → 4.1.2 / 5.3.2 上混已实现并验证**：FL/FR 直通、中置提取（5.3.2）、差信号环绕
   （延迟/带限/去相关）、全频高度、FL/FR mid 低音管理（简化；真矩阵 BM deferred），参数全部可调；全部用 CamillaDSP
   Mixer+Filter 表达，未写 Rust、未重造 MIMO 引擎。
   （2026-11-16 用户裁决：天空声道按普通全频声道处理，不做 1.8–10 kHz 带限、不做高通/低通、
   不做 18 ms 人工去相关延迟；预设与 examples 已同步修正，见 §2/§3。）
2. **两种交付形态**：channelmap v2 profile（WebUI 零改代码可用）+ 独立 CamillaDSP 配置
   （不依赖渲染器，SSH 热部署/文件后端测试）。
3. **测试通过**：本机 2026-11-16 复跑 6 预设×13 信号×2 路径 → 156/156 entry、480 项检查 0 失败；
   两条路径输出逐样本相同；与参考 DSP 仿真逐样本一致。板端 28/28 文件后端运行、64 项检查 0 失败、
   20s 音频上混 CPU 0.46–1.67s（单核 2–8%）——**板端数据为 2026-11-15 旧版（bandlimited height），
   改造后未复测，SUPERSEDED**。
4. 过程中发现并定位 **khaen-channelmap 渲染器 3 个影响所有 profile 的 P0 问题**，提供最小 patch，
   audio-stack 已合入（sha256 91e85e18…，OEM v5 已包含），并加回归测试。
5. **板端热部署已验证**（只写文件，不启停服务、不占声卡）：12 个 profile 安装、WebUI API 可见、
   沙箱 load-profile 渲染通过 `camilladsp --check`、apply/rollback 的 sha 校验一致。

## 2. 实现内容

| 组件 | 路径 | 说明 |
| --- | --- | --- |
| 生成器 | khaen-upmix | Python3+PyYAML；预设→DSP 计划→profile/独立配置；含 check/apply/rollback |
| 参数预设 | presets/upmix-presets.yml | 4.1.2/5.3.2 × music/movie/wide |
| 示例 | examples/ | 12 profile（含 *-safe）+ 6 个 ALSA CamillaDSP 配置 + manifest |
| 部署 | deploy/install-upmix.sh | 备份/安装/校验/自动回滚/卸载；不启停服务 |
| 测试 | tests/ | 信号生成、文件后端运行、参考仿真分析、板端计时 |
| 修复存档 | patches/ | khaen-channelmap 三处修复（已合入） |

上混设计（详见 docs/ALGORITHM.md）：

- 低音管理（现状）：SUB 只取 FL/FR 的 mid（每源 −6.02 dB），主箱 HP80；**C 经 HP200 后 80–200 Hz、SL/SR 经 HP100 后 80–100 Hz 未汇总、被丢弃**；`redirect_from: auto` 仅校验用、未生效；真矩阵 BM（C/SL/SR 重定向）deferred（C-20）。
- 上混：C=mid（5.3.2，200 Hz–12 kHz 带限，-6/-3 dB 预设可选）；SL/SR=(L−R)/2 + 100 Hz HP +
  7/8 kHz LP + 15/18 ms 延迟（可选同相填充与左右延迟差去相关）；HFL/HFR=mid 或左右混合，
  **按普通全频声道处理（无带限、无高通/低通、无人工去相关延迟；用户裁决 2026-11-16）**；
  5.3.2 三 SUB 同信号（可 per-SUB 微调；同信号 = 相干激励，非三倍平滑，A-P1-5 设计项 deferred）。
- 相位/梳状：每通道内部无时间错位副本；跨通道延迟 15/18 ms 是**有意包围感设计**，不是“避免梳状”的物理手段——两路仍线性相加、梳状仍在，只是听感转为空间感/回声（C-21/P1-4）；差信号天然去相关；
  高度为低电平全频信号（角色增益 music −9 dB / movie −6 dB；叠加 mid −6.02 后相对前置约 −15/−12 dB），
  靠电平差避免与前置等电平叠加；SUB 与主箱 80 Hz LR4 分频（仅 FL/FR mid）。
- 去相关：环绕默认左右延迟差；高度不做人工去相关延迟（用户裁决 2026-11-16）；
  可选 allpass Biquad 级联（仅独立配置，真实全通、幅度不变）。

## 3. 测试结果

### 3.1 本机（Windows，CamillaDSP 4.1.3 文件后端）

- 6 预设 × 13 信号 × 2 路径（standalone / channelmap 渲染）：**480 项检查 0 失败**。
- 该断言只证明“渲染器与生成计划一致”，**不证明 plan/映射本身正确**（plan 错仿真也会一致）；plan 正确性另由 role→物理通道期望表/映射断言与人工复核（A-P1-15）。
- 两条路径输出 **max|diff| = 0.00e+00**（逐样本相同，全部 preset/信号）。
- 与参考仿真误差 ≈ 1e-8（float32 量化级），说明 mixers/filters/delays 与设计完全一致。
- 电平/隔离/延迟示例（4.1.2-music，输入 -12 dBFS）：
  1 kHz 中心 → FL/FR -15.0、SL/SR -240、SUB -102.8、高度 -30.0（全频后比原带限 -51.4 高约 21 dB，符合带限删除）；
  仅左 → FL -15.0、FR -240、SL/SR -27.0；反相 → SL/SR -21.0、SUB -240；
  40 Hz → FL/FR -39.6、SUB -15.5。
- 相位/梳状：SL/SR 与 FL 的 0-lag 相关 ≈0.00，最大相关位于配置延迟（-15.1 ms）。

### 3.2 板端（RK3576，文件后端，不占声卡）

> **SUPERSEDED**：以下为 2026-11-15 旧版（bandlimited height + 旧渲染器）实测；2026-11-16 高度全频化后未板端复测。

| 项 | 结果 |
| --- | --- |
| 文件后端运行 | 28/28 成功（2 预设 × 7 信号 × 2 路径） |
| 输出分析（回传抽检） | 64 项检查 0 失败；两路径逐样本一致 |
| ALSA 配置 `camilladsp --check` | 6/6 "Config is valid"（不打开设备） |
| RTF / CPU（20 s 粉噪） | cs=128: RTF 1.33/2.07，CPU 1.59/1.67 s；cs=1024: RTF 13.5/11.8，CPU 0.75/0.62 s；cs=2048: RTF 19.3/16.5 |
| CPU 占单核 | 2.3–8.3%（8 核平台） |
| 自身延迟 | 最大 18 ms（环绕 movie/wide；高度 0 ms）+ chunksize/target_level 缓冲（文件后端不含 ALSA 实时等待） |

> 文件后端 RTF 不含 ALSA 实时等待/驱动开销，真机延迟与 32 帧能力需 MOTU 冒烟（由 audio-stack
> 执行）最终确认；按 CPU 余量判断实时无压力。

## 4. 渲染器问题（已修复）

发现 khaen-channelmap **build_camilladsp** 三处会改变音频结果的 bug（schema/校验正常，仅 render/apply）：

| # | 问题 | 影响 | 修复 |
| --- | --- | --- | --- |
| A | Filter step 语义用错：names 会被全部串到 channels 每个通道 | 所有带 per-role 分频/PEQ/FIR/gain/delay 的 profile；实测 4.1.2 的 FL 在 1 kHz 被衰减 -42 dB | 每 filter 一步（同 filter 多通道才合并） |
| B | roles[].sources[].inverted 渲染时被丢弃 | 差信号 (L−R) 变 mid (L+R) | 与 role polarity 异或后保留 |
| C | bass_management 的 bass_extract 未直通主声道（未映射输出=静音） | 开 BM 后主声道静音 | 先显式直通 0..pb-1 |

- 最小 patch：patches/khaen-channelmap-upmix-fixes.patch（基线 sha256 1e7dc232…）。
- audio-stack 已 review/合入，新增 3 条回归；新渲染器 sha256 **91e85e18…**，OEM v5 镜像已含。
- 修复后两条路径逐样本一致（本文 §3.1 数据）；本工具低音管理为 FL/FR mid 简化路径（不依赖渲染器 BM）。
- **旧镜像（<OEM v5）唯一可用路径 = 独立 CamillaDSP 配置（`khaen-upmix apply`）或先打 `patches/` 补丁；`*-safe` 只去源反相，不能绕过 bug A（所有 per-role 滤波仍会叠加）**（C-04）。

## 5. 热部署验证（板端 OEM v5）

- install-upmix.sh：12 profile 校验（真实 khaen-channelmap validate/render + 审计）全部通过；
  6 个 ALSA 示例 camilladsp --check 全部通过；无服务启停。
- WebUI（khaen-audio-web :8088）GET /api/channelmap 返回 12 个 upmix-* profile，布局正确。
- 沙箱 KHAEN_CHANNELMAP_* 重定向下 load-profile upmix-4.1.2-music --mode off 成功，渲染的
  配置 camilladsp --check 通过（"Config is valid"），未触碰 /etc active。
- 独立路径 khaen-upmix apply：A→B 后 rollback，sha256 与 A 一致；配置文件 camilladsp --check 通过。

## 6. 交付清单

- projects/khaen-audio-dsp/upmix/：khaen-upmix、presets、examples（12 profile + 6 配置）、deploy、
  tests、docs（ALGORITHM / WEBUI-INTEGRATION / TESTING）、patches、README、本报告。
- 板端已热部署：/usr/local/bin/khaen-upmix、/etc/khaen-audio/profiles/upmix-*.yml、
  /etc/khaen-audio/upmix/presets.yml、camilladsp-examples、install 备份。
- projects/khaen-audio-dsp/docs/upmix.md：给项目其他成员的入口说明。

## 7. 已知限制与后续

1. **MOTU 真机播放**未做（声卡由 audio-stack 冒烟）；需要时用 WebUI 加载 profile 或
   khaen-upmix apply 后实际听测，重点确认 4.1.2 映射（HFL/HFR=5-6、SUB1=8，1-based）与 5.3.2。
2. **H16 未确认**：本工具只处理 2ch 输入（pc-2ch）；16ch 容器输入、跨布局 remap 不在本任务范围。
3. **Dante 96k**：配置层可生成（--check），**未做 96k 文件后端/真机实测**；MOTU 22ch@96k 上限待核（A-P1-12）。
4. **allpass 去相关**仅独立配置支持（板端文件后端未跑，版本与本机一致）；channelmap profile 退化为固定延迟。
5. **中置策略**为静态提取（非自适应矩阵），大音量/强立体声素材下中心声像可能略偏；可用预设/增益调整。
6. 低音管理为 FL/FR mid 简化路径：C 80–200 Hz、SL/SR 80–100 Hz 未汇总（丢弃）；真矩阵 BM/redirect 生效/能量守恒检查 deferred（C-20）。


## 8. 输入内容模式（task-19，追加）

H16 固定 Pure、容器 7.1.4 但节目可能只有 2.0：新增输入内容检测与双路径切换。

- **判定**：只用 C/SL/SR/height 活动判多声道（**LFE 不作判据**：H16 旧固件会 L/R+LFE，仍应上混）；
  窗口/连续 2.5s + 迟滞 6dB + 启动冻结 2.5s；auto 默认直通，detected=stereo 才上混。
- **实现**：一个 CamillaDSP 双路径配置（上混路径 + 直通路径，Volume Aux1/Aux2 静音切换），
  daemon 读 CamillaDSP websocket `GetCaptureSignalRms`，写 `/run/khaen/input-content.json`，
  用 `SetFaderVolume` 切换（无重启、不打开声卡）；覆盖值写 `/etc/khaen-audio/input-mode.json`
  （auto|stereo|multichannel，接受 force-* 别名）。
- **CLI**：`detect --wav/--json`、`input-mode get|set`、`autodetect --simulate/--dry-run`、
  `autodetect-config --content-mode auto|stereo|multichannel`、`container -o DIR`。
- **测试**：本机 58/58 PASS；板端 OEM v5（RK3576/CamillaDSP 4.1.3 文件后端）58/58 PASS：
  4 种容器分类、LFE 判据、启动冻结、迟滞、override 优先级、别名、双路径文件后端
  （stereo 模式输出与 2ch 上混逐样本一致；pass 模式逐通道映射正确；C-only 在 stereo 模式静音；
  2.0 容器在 pass 模式只有 FL/FR 有能量）。mock websocket 另验证握手/命令/状态/fader 切换。
- **边界**：H16 设置无法直接读取，`h16_internal_upmix` 由内容推断；H16 开 Dolby Upmix/
  DTS Neural:X 时检测为多声道 → 板端直通（自动防双上混）。CamillaDSP 启动建议带
  `--gain1=-120 --gain2=0`（默认直通），daemon 接管后无叠加。
- 详见 `docs/INPUT-MODE.md`。

