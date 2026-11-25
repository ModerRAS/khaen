# 校准 YAML 契约 khaen-calibration/v1

> 状态：**设计侧规范 v1（2026-11-15）/ v1.1（2026-11-16）/ B2 口径修订（2026-11-17）**，由 upmix-dev 维护；Rust 实现（khaen-calibrate，projects/khaen-audio-dsp/calibrate/）由 audio-stack 主写。
> 2026-11-17 B2 修订（依据 `scratch/lead/doc-audit/CONSOLIDATED.md` C-13/15/16/17/18 与 B2 组 P1/P2）：修正 delay_samples 方向、补充验收入口与 holdout、注明校验器覆盖差异；字段语义不变。
> 本目录只放契约、验收口径、校验脚本与 review，不改 calibrate/ 代码。
> 变更规则：字段名/语义一旦发布不再改；新增只能 optional 并升 minor（v1.1），破坏性变更升 v2。

## 1. 作用与边界

校准链路分三段，本契约定义中间产物的格式与消费规则：

```
测量（khaen-calibrate，Rust）
  └─ 输出 khaen-calibration/v1 YAML（每通道 delay_samples / polarity / PEQ filters）
       ├─ 消费方 A：channelmap v2 profile roles[]（WebUI/渲染器）
       └─ 消费方 B：独立 CamillaDSP 配置（filter 链）
  └─ 应用后再测量（T3 必须，K4）+ holdout（T6）→ 按 ACCEPTANCE.md 验证（含合并响应/相位抵消）
```

非目标（本契约不定义）：

- 分频（HP/LP、crossovers）与上混矩阵——属于 profile/布局设计，不进校准文件。
- FIR/卷积校正、房间 EQ 的 target curve 设计。
- 自动应用/自动切换（校准文件只落盘；由 WebUI/用户确认后应用）。
- 群延迟/全通相位校正（v1 用延迟+极性+PEQ 近似；全通/全相位校正留 v2）。

## 2. 顶层结构

```yaml
schema: khaen-calibration/v1      # 必填，常量；v1.1 写 khaen-calibration/v1.1（见 §10）
sample_rate: 48000               # 必填，Hz（48000 / 96000）
reference_index: 0               # 必填；参考通道的播放通道号（channels[].index 之一）；推荐指向 delay_samples=0（到达最晚）的通道
layout: "5.3.2"                  # 建议必填；layout-presets.yml 的 output_layouts id
created_unix: 1794725000         # 建议；producer 写
tool: {name: khaen-calibrate, version: "0.1.0"}   # 建议；也接受 tool: "khaen-calibrate" + 顶层 version: "0.1.0"
note: "2026-11-15 客厅 5.3.2，UMIK-1 90° 校准/麦轴朝天花板，甜点单点"   # 可选，人读
measurement:                     # 可选（建议）；用于复现，不参与应用
  mode: direct                   # direct（推荐）| through_render
  sweep: {start_hz: 20, end_hz: 20000, seconds: 5.0, level_db: -30.0, fade_ms: 10}
  playback: {device: "hw:motu,0", channels: 22}
  capture:  {device: "hw:UMIK1,0", channels: 1}
  pre_seconds: 1.0
  post_seconds: 1.0
  repeats: 3
  render_offsets:                # 仅 mode=through_render 时必填
    - {name: SL, delay_samples: 720, polarity: normal}
channels:                        # 必填，非空；每个目标输出角色一条
  - name: FL                     # 必填；layout-presets 的角色 id（FL/FR/C/SUB1/HFL…），唯一
    index: 0                     # 必填；播放设备通道号（0-based），与布局映射一致
    delay_samples: 0             # 必填；施加到本通道的延迟补偿 D_i = round(max_j t_j - t_i)（t=实测到达）；min(D)=0，D=0 者为到达最晚的参考通道
    polarity: normal             # 必填；normal|invert
    gain_db: 0.0                 # 可选trim；-12..+6
    filters:                     # 必填可为空；PEQ 列表，≤32
      - freq: 45.0               # 20..min(20000, fs/2-1)
        q: 1.2                   # 0.1..10
        gain_db: -4.5            # -24..+24（单段）
    measured:                    # 可选证据；永不参与应用
      peak_db: -18.2
      snr_db: 42.0
      confidence: 0.97
      delay_raw_samples: 12
      polarity_raw: normal
      fr_csv: "FL-fr.csv"
      impulse_csv: "FL-impulse.csv"
```

### 2.1 字段表（规范）

| 字段 | 类型 | 必填 | 约束/语义 |
| --- | --- | --- | --- |
| schema | string | 是 | v1 文件必须等于 `khaen-calibration/v1`；新文件为 `khaen-calibration/v1.1`（差异见 §10） |
| sample_rate | u32 | 是 | 48000/96000；其他值工具可接受但消费方应警告 |
| reference_index | u32 | 是 | 参考通道的 `index`；必须存在于 channels[].index；**推荐指向 delay_samples=0 的通道**（否则校验器 warning；Rust 主入口待补，见 §5.1） |
| layout | string | 建议 | layout-presets id；写了下游必须校验 name/index 与布局一致 |
| created_unix | u64 | 建议 | producer 写 |
| tool | string 或 {name,version} | 建议 | 追溯用；字符串形式=工具名 |
| version | string | 否 | tool 的版本（与 tool 映射二选一/可并存） |
| note | string | 否 | ≤500 字 |
| measurement | mapping | 否 | 见 2.2；不参与应用 |
| channels | list | 是 | ≥1、≤32；`name` 唯一、`index` 唯一 |
| channels[].name | string | 是 | 角色 id，大写，形如 `^[A-Z][A-Z0-9]*$`；与 layout-presets 一致 |
| channels[].index | u32 | 是 | 播放通道号（MOTU 0..21；= 设备物理通道域，不是容器槽位号）。若 `measurement.playback.channels` 给出则必须小于它；必须与本次运行态 mapping 一致。**D8 已裁决**：4.1.2 = `FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7`（Line Out 8）；Line Out 10/槽 9 仅为历史临时测试，不是候选 |
| channels[].delay_samples | integer | 是 | `D_i = round(max_j t_j - t_i)`（施加到通道 i 的延迟补偿）；≥0；max=floor(0.2*fs)（200 ms 上限）；全文件 min 必须=0（D=0 的通道=到达最晚的参考通道） |
| channels[].polarity | string | 是 | `normal` / `invert`；兼容别名 `1`/`-1`（写文件前归一化） |
| channels[].gain_db | float | 否 | default 0；[-12,+6]；应用时与已有 role gain 相加 |
| channels[].filters | list | 是 | 可为 `[]`；≤32；元素见下；正提升总和按通道对 `filters[].gain_db` 正部求和（不含 trim，§5 规则 9） |
| filters[].freq | float | 是 | 20 ≤ freq < fs/2，且 ≤20000 |
| filters[].q | float | 是 | v1：0.1..10。v1.1 peaking：有限数 ≥0.1（窄模态可 >10）；v1.1 shelf：0.1..1.0（RBJ slope S；见 §3.4/§10） |
| filters[].gain_db | float | 是 | -24..+24 |
| filters[].type | string | 否 | v1 只允许 `peaking`（缺省=peaking）；v1.1 另允许 `low_shelf`/`high_shelf`（§10）；其他值消费方必须拒绝 |
| measured | mapping | 否 | 诊断；字段见 2.3，消费方忽略 |

### 2.2 measurement 块（复现信息，不参与应用）

> 现状（P2-3）：producer（`merge`/`fit`）当前**不写**本块，`through_render`/`render_offsets` 也没有实现路径；本块目前仅示例/手工记录存在。

| 字段 | 说明 |
| --- | --- |
| mode | `direct`（推荐：渲染层旁路/1:1，测物理链路）或 `through_render`（测含渲染层；必须给 render_offsets） |
| sweep | start_hz/end_hz/seconds/level_db/fade_ms |
| playback | device/channels（MOTU 播放 hw + 通道数） |
| capture | device/channels（UMIK-1 采集） |
| pre_seconds/post_seconds | 每次测量的静音保护 |
| repeats | 重复次数（建议 ≥3） |
| render_offsets | `[{name,delay_samples,polarity}]`；through_render 时必填，producer 从测得延迟中扣除渲染层延迟 |

### 2.3 measured 块（可选证据）

`{peak_db, snr_db, confidence(0..1), delay_raw_samples, polarity_raw, fr_csv, impulse_csv}`。
路径相对校准 YAML 所在目录；消费方不读这些文件，应用流程不依赖它们。

## 3. 语义与归一化

### 3.1 delay_samples（延迟补偿，不是“相对最早到达量”）

- **定义**：`D_i` 是**施加到通道 i 的延迟补偿**。producer 按实测到达时间 `t_i` 写
  `D_i = round(max_j t_j - t_i)`，输出前归一化到 `min(D) == 0`。
  因此 `D=0` 的通道是**到达最晚**的通道（参考通道），其余通道补上它与最晚者的差值。
  按“相对最早到达”的字面理解会得到负延迟（不可实现）或把方向弄反（双倍错位）——本契约以本节公式为准。
- 单位是**采样点**（`sample_rate` 下）；整数。次采样残差 < 0.5 sample；残余相位换算
  `Δφ ≈ 360°·f·0.5/fs`：80 Hz≈0.3°、2 kHz≈7.5°、20 kHz≈75°，故“交叉点 ≤30°”只在 fc ≤8.3 kHz（48k）成立
  （P2-1；更高 fc 按公式折算，不得直接套 30°）。
- 测量链公共延迟（DAC/ADC/功放/空气）在相对作差时抵消，不写入文件。
- 200 ms 上限来自 channelmap v2 `delay_ms` 0..200 约束；超过时工具必须报错而不是截断。
- `reference_index` 记录参考通道；**应指向 D=0（到达最晚）的通道**。若它指向 `D≠0` 的通道，说明 0 延迟通道是另一路（更晚到达）：
  校验器给 warning，producer 应把 `reference_index` 改为 D=0 的通道并在 `note` 说明。

### 3.2 polarity

- `normal` = 不反相；`invert` = 乘 -1（180°）。
- 极性在**所有 LTI 滤波之后**作用于该通道（与 LTI 可交换，位置不影响结果；文档固定为“输出级”）。
- 消费者合并规则：`final_polarity = render_polarity XOR calibration_invert`。
  例如 upmix 的差信号环绕（sources[].inverted）不改变 role polarity 字段；校准 invert 记在 role.polarity。

### 3.3 gain_db（可选 trim）与 boost 帽（D7，2026-11-17）

- 与已有 role gain **相加**，结果必须落在 channelmap 允许的 [-60,+12]。
- 用途：多通道电平对齐/安全余量；不是房间 EQ 主手段。
- **允许 +3 dB boost（D7）**：房间校准 PEQ/目标拟合可对房间谷做正向校正。规则（同时写入 §11 D7）：
  单段 `filters[].gain_db>0` ≤ **+3.0 dB**（producer 硬帽）；每通道正向总和 ≤6 dB（>6 warning）/≤12 dB（>12 拒绝）；
  **预测最大累计响应峰值 ≤ −6 dBFS（headroom ≥6 dB）**，实测/扫频/节目素材峰值 ≤ −1 dBFS；
  不满足时减少 boost 或加负 trim，不得靠降低播放电平规避。
- 规则口径（P2-2）：§5 规则 9 的“总正向提升”按**单个通道内 `filters[].gain_db` 的正部求和**，不含这里的 `gain_db` trim；shelf 的 `gain_db` 计入，trim 只在组合峰值/headroom 检查里另行考虑。

### 3.4 filters（PEQ）

- v1 只支持 **Peaking**（RBJ cookbook 双二阶，@gain_db@ 为峰值增益）；与 channelmap v2 `roles[].peq`（freq/q/gain_db，最多 32 段）一一对应。
- 频率按升序写（非强制但建议，便于 diff/阅读）。
- 多个 filter 串联；消费者按文件顺序应用，再应用 HP/LP（分频）与 gain/delay/polarity。应用链顺序见 §4。
- v1.1（2026-11-16 落地）：`type` 允许 `peaking`（缺省）、`low_shelf`、`high_shelf`；`highpass/lowpass/notch` 尚未定义，校验器按未知 type 拒绝。v1 消费方遇到未知 type 必须拒绝整份文件并报错，不做降级。
- **shelf 的 `q` 是 RBJ shelf slope S，不是 Q**（2026-11-16 裁决）：`alpha = sin(w0)/2 * sqrt((A + 1/A)(1/q - 1) + 2)`（`A = 10^(gain_db/40)`）；producer `fit` 默认 S=0.7。消费方自己实现 biquad 时必须用该 S 公式，不得套 peaking 的 Q 公式。CamillaDSP 映射见 §4.2。
- **字段名提示（P2-5/P2-6）**：`filters[].q` 在 shelf 行承载的是 slope S，阅读 schema 时不要当 Q。Q 型与 S 型是不同参数化，差值随 gain/fc 变化，**没有固定容差**（不要引用“0.48 dB”当许可）；两者禁止混用。
- **数值界限（裁决 5）**：v1.1 的 peaking `q` 为有限数且 ≥0.1（窄模态可 >10）；`low_shelf`/`high_shelf` 的 `q`=S 必须在 **[0.1, 1.0]**。S=1 对应 CamillaDSP `slope=12` dB/oct，超过则 `check_slope` 拒绝。因此片段里的 `slope=12*q` 恒 ≤12。

## 4. 消费方映射规则

### 4.1 → channelmap v2 profile（WebUI / khaen-channelmap 渲染器）

对每个 `channels[]`，找到同 `name` 的 `output.roles[]`：

```text
role.delay_ms   = render_role.delay_ms + delay_samples * 1000.0 / sample_rate
role.polarity   = XOR(render_role.polarity, polarity == "invert")
role.gain_db    = render_role.gain_db + calibration.gain_db
role.peq        = render_role.peq + [{freq, q, gain_db, type?} for f in filters]   # 按顺序追加
```

- `render_role.*` 是该 profile 在未应用校准时的值（例如上混 profile 的环绕 15 ms 艺术延迟必须保留）。
- role.delay_ms 必须在 0..200；超出则拒绝应用（不要静默截断）。
- 最多 32 段 PEQ；超出拒绝。
- `name` 在 profile roles 中不存在 → 拒绝并报错（防止把 5.3.2 校准误用到 4.1.2）。
- 不修改 input/remap/bass_management 等其它字段；不改 schema。
- `type` 必须原样写入：缺省 `peaking` 可省略；`low_shelf`/`high_shelf` 不能丢（丢 type = 静默降级成 Peaking，最大误差 ~3 dB，禁止）。
- 板端 `khaen-channelmap` 的 `peq` 支持 `type`（白名单 `peaking|low_shelf|high_shelf`，未知 type rc=2 拒绝整份 profile）。
  `q` 界限按 type 分流：peaking `[0.1, 10]`（channelmap 不接受 v1.1 契约里 peaking q>10，那种窄模态走 §4.2 片段）；
  shelf `[0.1, 1.0]`（越界 rc=3，不产文件）。渲染映射见 §4.2。
- **q>10 的分裂（P2-8）**：v1.1 契约允许 peaking `q>10`，`khaen-calibrate validate` 会 PASS；但 channelmap 路线整份拒绝，
  该文件**只能走 §4.2 CamillaDSP 片段路径**。Python 校验器对 q>10 给 warning；报告必须写清消费路径。
- `channels[].index` 是 MOTU 物理通道号（0-based），必须与本次运行态 mapping 一致；不是容器槽位号。
- 渲染器验证：`khaen-channelmap validate/render` 必须通过。

### 4.2 → 独立 CamillaDSP 配置

按角色输出通道构造 filter 链（顺序与渲染器一致）：

```text
Mixer → [HP/LP 分频] → Peaking×N（按文件顺序）→ Gain(role_gain+gain_db)
      → Delay(delay_ms, unit: ms) → polarity（Gain -1 或 mixer source inverted）→ 输出
```

- Delay 用 `unit: ms`（与 channelmap 渲染器一致）；若直接配置需要更精确，可用 `unit: samples, subsample: true` 表达次采样，但校准文件本身仍是整数 samples —— 该选项只用于实现层，不写入契约。
- Peaking filter 参数：`{"type":"Peaking","freq":f,"q":q,"gain":gain_db}`（CamillaDSP 字段名是 `gain`）。
- shelf filter 参数：**必须用 CamillaDSP 的 `Slope` 型**，`{"type":"Lowshelf|Highshelf","freq":f,"slope":12.0*q,"gain":gain_db}`
  （CamillaDSP 的 `slope` 单位 dB/oct，effective S = slope/12；故 `slope=12*q` 与 producer 的 S 系数完全一致）。
  用 `q` 字段会走 CamillaDSP 的 Q 型，与 producer 的 S 模型**不等价**（无固定容差，禁止）——本契约要求用 `Slope`。
- 极性实现：角色输出 Gain = -1（在 delay 后）；或 mixer 该路 source `inverted: true`。二者等价。

## 5. 校验规则（规范性）

校验器必须实现以下规则；错误=拒绝，warning=接受但提示。下列规则对 v1 文件生效；v1.1 仅在 `filters[].type` 与 `filters[].q` 上放宽（其余规则不变，见 §10）。

1. schema 精确匹配；YAML 顶层为 mapping。
2. sample_rate ∈ {44100,48000,88200,96000}（部署口径 48k/96k；其它值 warning）。
3. channels 非空、≤32；name 匹配 `^[A-Z][A-Z0-9]*$` 且唯一；index 唯一、≥0。
4. reference_index ∈ {c.index}；**应指向 delay_samples=0 的通道**（不指向=warning；Rust 主入口待补，见 §5.1）。
5. 每个 delay_samples 为整数、0 ≤ d ≤ floor(0.2*fs)；全文件 min == 0；数值语义见 §3.1（`D_i = round(max_j t_j - t_i)`，不是“相对最早到达”）。
6. polarity ∈ {normal,invert,1,-1}（±1 归一化）。
7. gain_db 缺省 0，[-12,+6]。
8. filters ≤32；每项 freq/q/gain_db 类型与范围合法；type 缺省或 `peaking`。
9. PEQ 正向提升按**单通道**对 `filters[].gain_db` 正部求和：Σmax(gain_db,0) ≤ 12 dB（>12 拒绝；>6 warning，D7 总增益帽）；**单段正向 > +3.0 dB = warning**（D7 单段 boost 帽，producer 必须钳制；Rust 主入口补齐后转 error）。不含 `channels[].gain_db` trim（P2-2）。
10. layout 存在时：若给 layout-presets，name/index 必须与 output_layouts[layout].roles 匹配（index 为该角色 outputs 的首个）。不匹配=error。
    若给 `measurement.playback.channels`，index 必须小于该值（设备通道域）；index 还必须与本次运行态 mapping 一致（P2-14；D8 已裁决：SUB1=槽位7/Line Out 8，Line Out 10 仅为历史临时测试，不是候选）。
11. measurement.mode=through_render 时必须有 render_offsets，且每个 offsets.name 在 channels 中；否则 warning。
    （现状：producer 不写 measurement，through_render 未有实现路径，见 §2.2。）
12. YAML 中未知顶层/通道字段：warning（保留扩展性）；但 `schema` 不匹配=error。
13. v1.1 且存在 peaking `q>10`：文件合法（PASS），但给 warning“仅 CamillaDSP 片段路径可应用；channelmap 会拒绝”（P2-8）。

### 5.1 校验器覆盖差异（C-P1-7，2026-11-17）

主入口 `khaen-calibrate validate`（Rust）与独立复核 `tools/validate_calibration.py`（Python）**并非全规则一致**：

| 规则 | Rust 主入口 | Python 独立复核 |
| --- | --- | --- |
| schema / sample_rate / channels / index 唯一 / reference 存在 | ✅ | ✅ |
| delay 范围 + min=0 | ✅ | ✅ |
| filters freq/q/gain/type（v1/v1.1） | ✅ | ✅ |
| name 唯一 | ❌（未查重） | ✅ |
| polarity 必填 + `1/-1` 别名 | ❌（缺省/别名静默通过） | ✅（缺省 warning；±1 归一化 warning） |
| reference 指向 delay=0 | ❌ | ✅（warning） |
| layout-presets 交叉校验 | ❌ | ✅（`--layout-presets`） |
| 未知字段 warning | ❌ | ✅ |
| measurement.render_offsets | ❌（不校验） | ✅ |
| q>10 的 channelmap 不可用 warning（v1.1） | ❌ | ✅ |

因此 **T0 = Rust 主入口 + Python 独立复核都通过**；在 Rust 补齐上述规则前，不得只凭 `khaen-calibrate validate` 签 T0。
`review/CALIBRATE-V11-VALIDATOR-RECEIPT.md` 的“一致”结论只覆盖已测 schema/type/q/freq 类用例。

## 6. 与现有 Rust 草稿（calibrate/src/model.rs）的差异

| 契约 | 草稿现状 | 需要的最小改动 |
| --- | --- | --- |
| schema/sample_rate/reference_index | 已有 | schema 值改为 `khaen-calibration/v1`；顶层加 tool/created_unix（可选） |
| channels[].name | 无（只有 index） | 加 `name: String`（角色 id） |
| delay_samples: i64 ≥0 | 已有 i64 | merge 写 `round(max t - t_i)` 并归一 min=0；fit（单通道）写 0（中间产物，待 merge 归一），方向定义见 §3.1 |
| polarity: normal/invert | 已改 String | 已符合 |
| filters | Vec<Peq{freq,q,gain_db}> | 即本契约 filters，字段一致（v1 仅 peaking；v1.1 增加 `type`，见 §10） |
| gain_db trim | 已有 Option<f64> | 已符合 |
| layout | 已有 Option<String> | 已符合；测量块/证据仍未入模型（optional） |
| tool/version | tool: Option<String> + version: Option<String> | 已兼容（契约接受字符串 tool + 顶层 version） |

## 7. 文件位置与生命周期（建议）

```text
/etc/khaen-audio/calibration/
├── 5.3.2-20261115-1800.yml     # 每次测量一个文件（只增不改）
├── 4.1.2-20261115-1810.yml
└── current.yml -> 5.3.2-...yml # 可选：WebUI 当前选中的校准
```

- producer 只写新文件（原子写：tmp+rename）；不覆盖已存在的历史文件。
- WebUI/用户选择后由渲染/应用流程读取；校准文件本身不触发应用、不重启服务。
- 应用前必须通过 §5 校验 + channelmap validate/render；失败回滚（与现有 profile apply 一致）。

## 8. 示例

见 `examples/calibration-5.3.2.example.yml`（5.3.2，含 3 个 SUB 与高度）与
`examples/calibration-4.1.2.example.yml`（4.1.2，SUB1 + 高度）。校验：

```bash
# 主入口（Rust；producer 自带）
khaen-calibrate validate examples/calibration-5.3.2.example.yml
# 独立复核（Python）
python3 tools/validate_calibration.py examples/calibration-5.3.2.example.yml \
  --layout-presets /etc/khaen-audio/layout-presets.yml
```

## 9. 消费方实现清单（checklist）

- [ ] 只读 YAML，不改 producer 的原始文件。
- [ ] 按 §5 校验；error 拒绝、warning 记录。
- [ ] 按 §3/§4 合并到目标 profile 或 CamillaDSP 配置（XOR 极性、延迟相加、PEQ 追加、gain 相加）。
- [ ] 应用前 camilladsp --check / khaen-channelmap validate+render。
- [ ] 应用后按 ACCEPTANCE.md **T3（必须，K4）**/T4 复测 + **T6 holdout**；negative_controls 5/5；保留应用前后曲线与报告。

## 10. v1.1（2026-11-16）

新写出的校准文件使用 `schema: khaen-calibration/v1.1`。v1 文件仍按旧规则校验。

| 项 | v1 | v1.1 |
| --- | --- | --- |
| `filters[].type` | 缺省且只能是 peaking | `peaking`（可省略）、`low_shelf`、`high_shelf`。不认识的 type 拒绝整个文件 |
| `filters[].q` | 0.1..=10 | peaking：有限数且 ≥ 0.1（窄模态可以大于 10）。shelf（`low_shelf`/`high_shelf`）：[0.1, 1.0]（RBJ slope S；CamillaDSP `slope=12*S` 上限 12 dB/oct） |
| 架滤波器 | 不允许 | producer 约定每通道最多各一段、Q 通常 0.7、计入宽带段数；校验器不强制（手写文件允许更多段，消费方按普通 biquad 处理） |
| 目标曲线 | 不是应用字段 | 不是应用字段，只出现在优化报告。**D4（2026-11-17）**：内置仅 `flat`（默认）与 `tilt10`（20 Hz 0 dB → 20 kHz −10 dB，log-f，≈−1.0034 dB/oct；参考增益独立于曲线形状）；旧 −3 dB@16k `tilt` superseded；其它曲线仅通过导入（`@file#id`）扩展、不设默认（§11） |

只含 Peaking 且 Q ≤ 10、且文件本身无 error 的 v1.1 文件，校验会注明「v1 可读取」；含架滤波器或任一 `q` > 10 的 v1.1 文件不再给该提示（v1 消费方读不了）。

v1.1 的 peaking `q>10` 在契约内合法（validate PASS），但 **channelmap 应用路线会拒绝整份文件**：这种文件只能走 §4.2 的 CamillaDSP 片段；Python 校验器给 warning（P2-8）。

「架滤波器最多各一段」是 producer 端约定（`fit`/`merge` 的 `peq::maybe_shelves` 自动满足），两个校验器都不强制：消费方不得据此假设最多一段，应按 §4 追加所有已知 type 的 biquad。

校验入口：`khaen-calibrate validate` 为主入口（producer 自带、板端可跑）；Python 校验器保留为独立复核（支持 v1/v1.1，不依赖 Rust 二进制，可加 `--layout-presets` 做映射交叉校验）：

```bash
khaen-calibrate validate calibration.yml
khaen-calibrate check-pair --dir results --calibration calibration.yml --pairs FL:SUB1:80
# 独立复核（Python）
python3 calibration-spec/tools/validate_calibration.py calibration.yml \
  --layout-presets /etc/khaen-audio/layout-presets.yml
```

CamillaDSP 片段把 type 写成 `Peaking` / `Lowshelf` / `Highshelf`。拟合目标是把实测校正到目标曲线，默认平坦。

## 11. 2026-11-17 用户裁决（落地口径）

1. **D8（SUB 物理口，终局）**：4.1.2 的 SUB1 唯一物理口 = **Line Out 8（槽位 7，0-based index=7）**，即 `FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7`（槽 6 空）。
   Line Out 10（槽 9/ch9）只是当时 Line Out 8 不出声的**临时测试接法，不是设计候选**：不得再作为待核实项/二选一，不得写 index=9；历史说明仅此一句。
2. **D3（5.3.2 中置）**：中置按设计与主箱相同为**全频**（与 K2“高度全频”同口径）。当前无中置，故 C 的 HP80/低频重定向、C:SUB pair、C 相关测试**全部 deferred（NEEDS_HW，不在 P1 验收内）**。
   4.1.2 的 T4 pair 仅 `FL:SUB1:80`、`FR:SUB1:80`；5.3.2 主箱用“SUB 组等效”成对，不得写 `C:SUB`。
3. **D7（允许 +3 dB boost）**：房间校准 PEQ/目标拟合允许正向校正，规则：
   - 单段 `filters[].gain_db>0` ≤ **+3.0 dB**（producer 硬帽；`fit/merge` 的 boost cap 以此为准，当前 `--boost-cap-db` 默认 0 需 audio-stack 按裁决调整）；
   - 每通道正向总和 Σmax(gain,0)：>6 dB warning、>12 dB 拒绝（总增益帽，不含 `channels[].gain_db` trim）；
   - **削波/headroom**：应用后预测最大累计响应峰值 ≤ **−6 dBFS**（headroom ≥6 dB），实测/扫频/节目素材峰值 ≤ **−1 dBFS**；不满足时减少 boost 或加负 trim，不得靠降低播放电平规避；
   - 仍不可填（>+3 dB）的谷列 `unfilled_dips`，不作 FAIL；T3 目标偏差 ±3 dB 的统计频段包含 ≤+3 dB boost 区。
4. **D4（目标曲线，终局）**：内置目标曲线只有 **flat（默认）** 与 **tilt10**（20 Hz 0 dB → 20 kHz −10 dB，对数频率常数斜率 ≈−1.0034 dB/oct；参考增益独立于曲线形状）。
   旧 `tilt`（−3 dB@16k）**superseded**，不再作为默认/可用目标；其它曲线只能通过导入（`@file#id`）扩展、不设默认、不进交付验收。
   > 工程侧：`calibrate/target-curves.yml` 仍含旧 `tilt` 点表，需 audio-stack 按本裁决更新为 flat+tilt10（该文件不在本任务写域）。
