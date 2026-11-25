# 上混算法与参数说明（2.0 → 4.1.2 / 5.3.2）

本文描述 khaen-upmix 生成的 DSP 拓扑、数学定义、参数含义，以及哪些地方存在/不存在
梳状与相位抵消（**跨通道梳状仍存在**，见 §4）。实现全部落在 CamillaDSP 的 **Mixer + Filter** 上（无自研 MIMO 引擎）。

## 1. 总体拓扑

```
2ch 输入 ──> [upmix mixer: 2×N 矩阵] ──> [每角色滤波链] ──> N 声道输出
                                       HP → LP → (可选 allpass Biquad) → Gain → Delay
```

- **upmix mixer** 是一个 2×N 的实数矩阵：每行（输出角色）是 FL/FR 的加权和，权重以 dB 表示，
  负号用 `inverted` 表达。因此所有"提取/混合/反相"在数学上都是线性组合。
- **每角色滤波链**只做该角色需要的带限与时间处理：高通、低通、增益、延迟（以及可选的
  allpass 去相关）。天空声道按用户裁决（2026-11-16）按普通全频声道处理，默认不做高通/低通、
  不做人工去相关延迟（见 §3.4）。
- 采样率由模式决定（AES67 48k / Dante 96k）；profile 不含采样率相关依赖（无 FIR 文件），配置层可生成，
  但 **96k 未做文件后端/真机实测**（A-P1-12）；MOTU 22ch 在 96k 下的通道上限待核，不要写成“已验证成立”。

## 2. 输入/输出布局

输入固定 `pc-2ch`（FL=ch0, FR=ch1）。输出布局读取 `/etc/khaen-audio/layout-presets.yml`：

| 布局 | 角色与输出通道（0-based） |
| --- | --- |
| 4.1.2 | FL=0 FR=1 SL=2 SR=3 HFL=4 HFR=5 SUB1=7（ch6 空；8 个逻辑槽位，设备仍 MOTU 22ch） |
| 5.3.2 | FL=0 FR=1 C=2 SL=3 SR=4 SUB1=5 SUB2=6 SUB3=7 HFL=8 HFR=9 |

- **唯一真相**：以上映射以板端 `audio/overlay/etc/khaen-audio/layout-presets.yml` 为准（task-19 裁决：4.1.2 为 HFL4/HFR5/SUB1=7，ch6 空；ch6 保留无更多推导，按板端部署表执行）。
- **逻辑声道 vs 物理设备**：channelmap profile 的 `output.channels` 是逻辑声道数（4.1.2=8、5.3.2=10）；独立 CamillaDSP 配置固定按 MOTU 22ch 打开，其余设备通道恒静音（P2-6）。

## 3. 各通道合成

记输入瞬时样本为 L、R，增益均以 dB 表示，g = 10^(dB/20)。

### 3.1 FL / FR（直通）

```
FL_out = L × g_front          FR_out = R × g_front
```

- `front.gain_db = 0` 且 bass 关闭时逐采样直通（bit-exact）；**预设默认 bass.enabled=true**，FL/FR 必过 80 Hz LR4 HP，所以实际不是 bit-exact（P2-1）。
- bass 启用时 FL/FR 加 80 Hz LR4 高通（低音去 SUB），1 kHz 以上仍与输入一致。

### 3.2 C（中置，仅 5.3.2）

```
mid    = g_extract × (L + R)          # g_extract = -6.02 dB → mid = (L+R)/2
C_out  = mid × g_center               # 再经 200 Hz LR4 HP + 12 kHz LR4 LP
```

- 中心声像内容（L≈R）在 C 上保留，边缘声像内容被 mid 抵消。
- 带限 200 Hz–12 kHz 是历史保守设置（艺术/听感取舍，不是物理必需）；**C 的低频不汇总到 SUB**（见 §3.5），80–200 Hz 实际被丢弃。
- 中心内容在 FL/FR/C 同相叠加：C 为 −6 dB 时中心席约 +7.5 dB 相对单箱（静态提取取舍，未做 steering/center width）。
- 不做自适应矩阵（Pro Logic 式 steering）：静态提取更可预测，不会随内容"泵动"。
- 4.1.2 无 C：中心内容仍在 FL/FR（虚拟中置）。

### 3.3 SL / SR（环绕）

```
diff  = g_extract × (L − R)           # g_extract = -6.02 dB → (L−R)/2
SL_out = g_surround × LP7k(HP100(diff + amb × (L+R))) × delay(15 ms)
SR_out = g_surround × LP7k(HP100(−diff + amb × (L+R))) × delay(15 ms + spread)
```

- 差信号 =(L−R)/2：中心内容（L=R）在环绕上为 0，只有声像/空间信息进入环绕。
- `ambience_gain_db`（可选，movie/wide 预设为 −15/−18 dB）把少量同相 mid 加进环绕，
  避免纯差信号在窄声像素材上"太空"。
- 低通 7–8 kHz：去掉会拉偏声像的齿音/高频；高通 100 Hz 保护小环绕箱。
- 延迟 15/18 ms 是**有意设计选择**（制造包围感/空间感，代价：瞬态/声像偏移）；**延迟不能消除梳状**——两箱在听音点仍线性相加，梳状由声程差决定，加延迟只搬移图案/改变可听性（C-21（原 A-P1-4/A-P2-10））。
- 低通 7–8 kHz 为历史保守设置（Pro Logic 时代窄带环绕沿革），现代离散环绕非必需；依据未验证，标“艺术/保守选择”，设计优化 deferred。
- `delay_spread_ms`（wide 预设 3 ms）：右环绕额外延迟，左右不相关 → 更宽、更扩散。

### 3.4 HFL / HFR（高度）

```
mid  = (L + R)/2
HFL_out = g_height × (mid + width 混合项)
HFR_out = 同上（左右对称）
```

- 默认 `source=mid`、`width=0`：双高同信号（单声道高度）。
- **用户裁决（2026-11-16）**：天空声道按普通全频声道处理，**不做高通/低通、不做人工去相关延迟**；
  预设默认 `mode=broadband`、`delay_ms=0`、`delay_spread_ms=0`，高度只做电平/矩阵合成。
- `width>0` 时把左右直达按比例混入（movie 0.3 / wide 0.5），高度也有左右差异。
- 如需带限（实验/特殊摆位），必须在预设里显式写 `mode: bandlimited` + `highpass/lowpass`；
  默认不启用（不按角色硬编码任何带限）。

### 3.5 SUB（FL/FR mid 低音管理；真矩阵 BM 未实现）

```
FL/FR_out 先经 80 Hz LR4 高通（保护主箱）
SUBn_out = g_sub × LP80(LR4)( g_extract × (L + R) )   # 每源 −6.02 dB → SUB=LP((L+R)/2)
```

- **现状（事实，C-20）**：SUB 的 sources 固定只取 **FL/FR 的 mid**（每源 −6.02 dB → SUB=LP((L+R)/2)）；
  预设里的 `bass.redirect_from: auto` **仅校验用、未生效**；C 经 HP200 后 80–200 Hz、SL/SR 经 HP100 后 80–100 Hz **未汇总、被丢弃**。
  因此“矩阵式低音管理/所有主声道交 SUB/已与 BM 等价”的说法不成立；真矩阵 BM（C/SL/SR 重定向、能量守恒检查）为 deferred，不在本任务改代码。
- **不使用** CamillaDSP/channelmap 的 `bass_management` 路径（渲染器 BM 在 OEM v5 已修复；旧镜像需打 patches/；本工具默认走上面的简化路径）。
- 环绕/中置低频不汇总：2ch 上混里环绕低频多来自差信号，汇总会把反相内容灌进 SUB；当前实现按各自高通保护。
- 5.3.2 的 SUB2/SUB3 默认与 SUB1 同信号；`sub_trims` 可给每个 SUB 单独的增益/延迟，
  用于多低音摆位的对齐（例如 SUB2 −2 dB / +2 ms）。
- 电平：`extract_gain_db=-6.02` 是"平均"口径；设为 0 则 L/R 相关的低频在 SUB 上叠加 +6 dB，
  0 dBFS 素材需留余量（见 §5）。
- **直通/容器下混路径（passthrough）不含低音管理**：FL/FR 全频、SUB 只收 LFE，与上混路径（FL/FR HP80 + SUB=LP mid）低频行为不同；切换 stereo/multichannel 会有低音差异（A-P1-10）。

### 3.6 去相关（可选）

1. **延迟差**（默认仅环绕）：`delay_spread_ms` 让左右环绕有轻微时间差，听感更宽；
   天空声道按用户裁决不再使用人工去相关延迟（`height.delay_ms=0`、`delay_spread_ms=0`）。
2. **allpass 级联**（仅独立 CamillaDSP 配置）：每去相关角色串 4 个 Allpass Biquad（不同频点/Q，
   左右/前后不同参数）。真实全通 → **幅度响应不变**，只改变相位，实测 SL–SR 相关从 −1.00 降到
   −0.95（粉噪），且各通道电平完全不变。channelmap profile 无 Allpass 字段，故 profile 路径退化为
   固定延迟（会打印提示）。

## 4. 梳状/相位抵消：哪些存在、哪些不存在

1. **同一个输出通道内不会叠加时间错位的同一信号**：每角色只有一条滤波链；加权和发生在混音矩阵里
   （同一样本对齐），不会在通道内部产生"直达 + 延迟副本"的梳状。
2. **跨通道延迟 ≥ 15 ms（环绕）**：这是有意的包围感设计，不是“避免梳状”的物理手段——两路在听音点仍线性相加，梳状仍在；延迟只把听感从“音色染色”转为“空间感/回声”，代价是瞬态/声像偏移（C-21/P1-4）。
3. **差信号天然去相关**：SL/SR 是 L−R，与 L/R 的宽带相关接近 0（实测 r(lag0) ≈ 0.00），
   不存在与前置同相叠加导致的 0 lag 抵消。
4. **中置只提取 mid、不做反相混合**：C 与 L/R 同相；C 是独立音箱，不与 L/R 电学求和。
5. **高度为低电平全频信号**：不做带限、不做人工延迟副本；角色增益 music −9 dB / movie −6 dB，
   叠加 mid 每源 −6.02 dB 后，相对前置总电平约 **−15 dB（music）/ −12 dB（movie）**（实测 HFL −30 vs FL −15），
   靠电平差避免与前置等电平叠加，而不是靠带限（D-P1-2）。
   跨通道差异主要由电平与 width 混入决定。
6. **SUB 与主箱分频**：80 Hz LR4 高通 + 80 Hz LR4 低通，同相相加近似平坦；SUB 只收 mid，
   避免差信号低频反相注入。

## 5. 电平与余量

- 单声道同相内容（L=R=s）按代码实际：FL/FR 输出 s；C（5.3.2）= s×g_center（源 2×−6.02 → mid=s）；
  高度 = 0.5s×g_height（代码 base 每源 ×0.5）；SUB = s×g_extract（相干时 mid=s，不是 ×√2 的非相干公式）。
  5.3.2-movie 下总声压比纯 2.0 高约 3–5 dB，建议系统总音量/限幅器留 6 dB 余量（A-P1-9）。
- 0 dBFS 粉噪/满幅素材：FL/FR 不变；SUB 可能 >0 dBFS（数字域），输出格式为整数时会被 DAC 削波。
  建议源电平 ≤ −6 dBFS，或在 CamillaDSP pipeline 末尾加 Limiter（独立配置里可自行加）。
- 实测（−20 dBFS RMS 粉噪，2026-11-16 全频高度）：FL/FR ≈ −24 dBFS；SL/SR ≈ −34 dBFS；
  HFL/HFR ≈ −38 dBFS（music 预设；原带限版本约 −51 dBFS，全频后能量更高，符合预期），
  与设计增益一致（参照 REPORT.md / scratch/lead/height-policy/local-report.md）。

## 6. 参数表

| 参数 | 默认（music） | 范围 | 含义 |
| --- | --- | --- | --- |
| front.gain_db | 0 | [-60,12] | FL/FR 直通增益 |
| center.gain_db | -6 | [-60,12] | 中置总增益（5.3.2） |
| center.extract_gain_db | -6.02 | [-60,12] | 每输入进 mid 的增益 |
| center.delay_ms | 0 | [0,200] | 中置延迟（对齐用） |
| center.highpass/lowpass | 200/12000 Hz LR4 | 20–24k | 中置带限 |
| surround.gain_db | -6 | [-60,12] | 环绕总增益 |
| surround.extract_gain_db | -6.02 | [-60,12] | 每输入进差信号的增益 |
| surround.ambience_gain_db | null | [-60,12] | 同相 mid 填充（null=关） |
| surround.delay_ms | 15 | [0,199] | 环绕延迟 |
| surround.delay_spread_ms | 0 | [0,50]（合计≤200） | 右环绕额外延迟（去相关） |
| surround.highpass/lowpass | 100/7000 Hz LR4 | 20–24k | 环绕带限 |
| height.mode | broadband | bandlimited/broadband | 全频（默认）；bandlimited 需显式配置 |
| height.source | mid | mid/side | 高度来源 |
| height.gain_db | -9 | [-60,12] | 高度总增益 |
| height.extract_gain_db | -6.02 | [-60,12] | 每输入进 mid 的增益 |
| height.width | 0 | [0,1] | 左右直达混入比例 |
| height.delay_ms / delay_spread_ms | 0 / 0 | 同环绕 | 默认不做人工去相关延迟 |
| height.highpass/lowpass | 不启用 | 20–24k | 仅显式 mode=bandlimited 时使用；默认全频（用户裁决 2026-11-16） |
| bass.enabled | true | bool | 低音管理开关 |
| bass.mode | matrix | matrix/redirect | matrix=简化路径（SUB 仅 FL/FR mid）；redirect=渲染器 BM（OEM v5 起修复；旧镜像需打补丁） |
| bass.extract_gain_db | -6.02 | [-60,12] | 每主声道进 SUB 的增益 |
| bass.mains_highpass | 80 Hz LR4 | 20–24k | 主声道高通 |
| bass.sub_lowpass | 80 Hz LR4 | 20–24k | SUB 低通 |
| bass.sub_gain_db | 0 | [-60,12] | SUB 额外增益 |
| bass.sub_trims | [] | 每 SUB {gain_db,delay_ms} | 多低音对齐 |
| decor.mode | delay | delay/allpass | allpass 仅独立配置 |

## 7. 与 WebUI/channelmap 字段的对应

| 上混参数 | channelmap v2 profile 字段 |
| --- | --- |
| FL/FR 直通 | roles[FL/FR].sources + gain_db |
| C 提取 | roles[C].sources=[FL,FR]（各 −6.02）+ gain_db + highpass/lowpass |
| 环绕差信号 | roles[SL/SR].sources（含 inverted）+ gain_db + delay_ms + 高通/低通 |
| 高度 | roles[HFL/HFR].sources + gain_db（全频；默认无 delay/带限） |
| SUB | roles[SUBn].sources=[FL,FR] + lowpass + gain_db + delay_ms |
| 主箱高通 | roles[FL..].highpass |
| 去相关（延迟差） | roles[SL/SR].delay_ms 差值（高度不再使用） |
| allpass | 不适用（独立配置专用） |

全部字段都在现有 v2 schema 内，**不新增任何 schema 字段**。
