# v1.1 消费方缺口裁决（2026-11-16，Lead）

> 依据：`scratch/lead/web-v11/REPORT.md`（web-consumer 审计，293 行）、`scratch/lead/fr-repeat/REPORT.md`（audio-stack 根因）、
> Lead 独立复核（CamillaDSP 源码/S24 原始采集/cross-correlation）。本文件是 task-12/13/14 的唯一口径来源。

## 裁决 1：channelmap v2 路线支持 `peq[].type`（选项 A）

理由：CONTRACT §4.1 是板端原生应用路径（渲染器自带校验、Web UI 走它）；限 Peaking-only 会挡住 tilt 目标（`high_shelf`）
与 Q>10 窄模态，且"丢 type 静默降级"（最大 ~3 dB）不可接受。

要求：
- 板端 Python `khaen-channelmap`：`PEQ_KEYS` 加 `type`（可省，缺省 `peaking`）；白名单 `peaking|low_shelf|high_shelf`；
  遇到其它 type = **schema error rc=2**（不降级）；q 规则见裁决 2；`render` 按 type 输出
  `Peaking{freq,q,gain}` / `Lowshelf|Highshelf{freq,slope,gain}`（`slope = 12*q`）。
- `q` 界限：peaking `[0.1,10]`（保持）；shelf 亦 `[0.1,10]`（q=RBJ slope S，见裁决 2）。超出 = 明确报错，不 clamp。
- CONTRACT §4.1：`role.peq` 追加项带 `type`；注明含 shelf 的 v1.1 可走 channelmap，渲染器负责映射；
  转换器/手工不得丢 `type`（丢 = 静默降级，禁止）。
- producer 侧配套（task-12）：`fit/merge` 的模态 Q 上限钳到 **10**（与 channelmap 对齐），`q_unresolved` 一律不写滤波器。

## 裁决 2：shelf 的 `q` = RBJ shelf slope S（保持 producer 语义），CamillaDSP 片段改写 `slope: 12*q`

依据：`peq.rs:77-81` 的 `alpha = sin(w0)/2 * sqrt((A+1/A)(1/q - 1) + 2)` 就是 RBJ cookbook 的 S 公式（`fit` 默认 S=0.7）；
CamillaDSP `Slope` 型的公式是 `sin/2*sqrt((A+1/A)*(1/(slope/12)-1)+2)`（`biquad.rs:162-181,208-227`），故 `slope=12*q` 与 producer
**系数完全一致**；写成 `q:` 走的是 Q 型（`beta=sin*sqrt(A)/q`），同参最多差 0.48 dB。选择改片段映射而不是改 producer，
是为了不推翻已签收的 Round-7 数字（`predicted.csv`/check-pair 都用 S 公式）。

要求：
- `cal_to_camilladsp`（`main.rs:1085-1117`）：peaking 仍 `{"type":"Peaking","freq","q","gain"}`；
  shelf 改为 `{"type":"Lowshelf|Highshelf","freq","slope": 12.0*q,"gain"}`。
- CONTRACT §3.4/§4.2 写清：shelf 的 `filters[].q` 是 RBJ slope S；→CamillaDSP 用 `slope`（dB/oct，=12S）；
  消费方若自己实现 biquad，必须用 RBJ shelf 的 S 公式，不得套 peaking 的 Q 公式。
- `check.rs`/`peq.rs`/predicted.csv/pair 判据的模型不变（本来就是 S）。
- `khaen-channelmap` 渲染 shelf 同样用 `slope = 12*q`（见裁决 1）。

## 裁决 3：板端 Python `khaen-channelmap` 是唯一权威；Web 只委托 + 展示

要求：
- Web 后端本地回退必须与 Python 规则一致（type 白名单、q 界限、shelf 映射）；**不一致时禁止放行**：
  若 CLI 缺失/不可用，返回明确错误（"khaen-channelmap 不可用，无法校验/渲染"），不得用本地规则替代。
- `web/dist` 二进制作废（11-15 构建，grep 无 `low_shelf/high_shelf`）：重编译后再上板，同时带入 `MAIN_TRIM_MAX` 补丁。
- Web UI（P2，不阻塞）：PEQ 行加 `type` 只读显示 + 下拉（默认 peaking），保存/apply 前能看到 shelf。
- 无校准文件导入路由（P2，后置；先走 CLI+profile）。

## 责任划分

| 任务 | 负责人 | 写作用域 |
| --- | --- | --- |
| task-12：calibrate P0（A 根因 snap + B fit_clock 鲁棒 + P0-2 模态过拟合 + P0-3 SUB SNR + P0-4 position_usable）+ 片段 shelf slope + validate 提示口径 + 构建 | audio-stack | `calibrate/` |
| task-13：板端 `khaen-channelmap` peq.type 支持 + slope 映射 + 测试/文档 | upmix-dev | `audio/overlay/usr/local/bin/khaen-channelmap`、`upmix/docs` |
| task-14：Web 对齐 + UI type 显示 + 重编译 | web-consumer | `web/` |
| CONTRACT §4.1/§4.2 更新、裁决记录、板端复测 | Lead | `calibration-spec/`、板端 |

## 验收（本裁决）

1. `khaen-channelmap validate/render` 对含 shelf 的 profile：rc=0，产物含 `Lowshelf/Highshelf` + `slope=12q`；
   未知 type：rc=2 且不产文件；q 越界：rc=3。
2. CamillaDSP `--check` 通过含 shelf 的渲染配置；producer 片段 shelf 用 `slope`（不再用 `q`）。
3. Web 重编译后：板端 validate/apply 全部委托 Python；UI 能看到 type。
4. 回归：既有 peaking-only profile/render/校验不回归（upmix 960/480 检查仍绿）。

## 裁决 4（2026-11-16 追加）：clock_ppm 的验收口径 —— 「估计值」不是「漂移证据」

依据：upmix-dev task-9 独立复算（`scratch/lead/verify-fr-opt/REPORT.md`）+ Lead 复核。

事实：
- 参考麦 REF（MOTU ch0，MA 1）与播放共用 MOTU 时钟（同一设备），**真实漂移应为 0**；但当前 3 遍 × 5 s（跨度 10.4 s）
  的拟合给出 `clock_ppm=+12.84, ok=true`。3 遍时 1 个样本的时间误差 ≈ 2 ppm（499200 样本跨度），
  12.84 ppm ≈ 6.4 样本跨度误差 —— 与单遍起点受 60 ms 反射干扰造成的几样本偏差同量级。
- `fit_clock` 的残差即使只有 3.2 样本，端点/回归斜率仍会把它放大成 12.8 ppm；残差门（0.25×间隔=62400）完全挡不住。
- UMIK 的 −1.86 ppm 同理不可当"漂移"引用（分辨率 ~2 ppm/样本）。

裁决（写进 producer 与验收报告口径）：
1. producer 增加 **ppm 不确定度**：由拟合残差 + 跨度估计 `clock_ppm_uncertainty_ppm`；
   只有 `|ppm| > 2×uncertainty` 才置 `clock_ppm_significant=true`（`clock_ppm_ok` 仍表示"可估计"，不表示"已证实漂移"）。
   `--drift-correct` 只在 significant 时启用，否则跳过并 WARN（避免把噪声当漂移重采样）。
2. 3 遍/2 个间隔时 ppm 只能当"参考值"；验收报告**不得**引用 REF 的 +12.84 作为"漂移已修正/已验证"。
   需要真实漂移请用 ≥5 遍或更长间隔（或直接测同一信号在两时钟域的长度差）。
3. 加 **跨遍 IR 峰一致性门**：`ir_peak_idx` 跨遍偏差 > ~200 样本的那一遍标 suspect（robust-t4 REF repeat0 偏 2874 样本 = 60 ms 反射；
   robust-t4 UMIK FR 有 ±1534 ±交替误锁但旧判据给了 `clock_ppm_ok=true`）——这类坏遍不能进 ppm/延迟主聚合。
4. 修正 `calibrate/docs/ROBUST-POLARITY-RESULT.md` 第 282 行表格：REF `+12.84, ok=true` 改为
   "估计值 +12.84 ppm、与估计不确定度同量级、不可当物理漂移；栅格修复的验收依据是三遍起点落在同一栅格（延迟散差 ≤3.2 样本）与假峰 10741 被剔除"。

## 裁决 5（2026-11-16 追加）：shelf 的 q 收紧为 [0.1, 1.0]（CamillaDSP slope 上限 12 dB/oct）

事实：CamillaDSP 4.1.3 `filters/biquad.rs` 的 `check_slope` 拒绝 `slope > 12.0`（"Slope must be <= 12.0"）。
我们的映射是 `slope = 12*S`，所以 **S>1 的 shelf 无法被 CamillaDSP 执行**；当前 channelmap/web 允许 q∈[0.1,10]，
会出现 "validate 通过、render/--check 失败（rc=4）" 的 fail-closed 但难用的状态。

裁决（三处一致，supercedes 裁决 1/2 里的字面 q 界限）：
- `filters[].q`：**peaking** 保持 `[0.1, 10]`（channelmap）/ `≥0.1 无上限`（v1.1 契约，窄模态）；**shelf（low_shelf/high_shelf）改为 `[0.1, 1.0]`**（RBJ slope S；S=1 即 12 dB/oct，CamillaDSP 上限）。
- 板端 `khaen-channelmap`：shelf q 超界 = **rc=3**（error，不产文件），删除 "q>1 仅 warning" 的做法。
- Web 本地回退、Python 复核校验器、Rust 校验器（audio-stack）同步；producer 的 `fit` shelf 默认 S=0.7，天然满足。
- CONTRACT §2.1/§3.4/§10 按此改写；CamillaDSP 片段 `slope=12*q` 因此恒 ≤12。

理由：v1.1 契约是"可被本系统执行"的契约；允许 S>1 等于容忍一份任何消费方都渲染不出来的文件。
