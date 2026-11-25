# peq[].type：profile 的 peaking / shelf 语义与 CamillaDSP 映射

> 依据：Lead 裁决 2026-11-16（`calibration-spec/review/V11-CONSUMER-DECISIONS-2026-11-16.md` 裁决 1/2/5）、
> CONTRACT §3.4/§4.1/§4.2。实现：`audio/overlay/usr/local/bin/khaen-channelmap`（板端唯一权威）。

## 1. 字段与校验

`output.roles[].peq[]` 每项：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| freq | 是 | Hz，[20, min(48000, fs)/2) |
| q | 是 | peaking 时是 Q，界限 [0.1, 10]；shelf 时是 RBJ shelf slope **S**，界限 [0.1, 1.0] |
| gain_db | 是 | dB，[-24, 24] |
| type | 否 | `peaking`（缺省）\| `low_shelf` \| `high_shelf` |

规则：

- 缺省 type = `peaking`；归一化后的 peq 对 peaking 省略 type（与 producer `Peq` 的 serde 一致）。
- 未知 type（如 `notch`、拼写错误）= **schema error，rc=2，整份 profile 拒绝，不产出文件**；
  不降级为 Peaking（丢 type 静默降级最大误差 ~3 dB，裁决明确禁止）。
- q 越界 = rc=3，不 clamp、不降级为 warning；freq/gain 越界同样 rc=3。
  peaking 与 shelf 的界限不同（见上表，裁决 5）。
- 最多 32 段 PEQ。

## 2. q 的语义

- `peaking`：q 是 RBJ peaking 的 Q。
- `low_shelf`/`high_shelf`：q 是 RBJ shelf 的 slope **S**（不是 Q）：
  `alpha = sin(w0)/2 * sqrt((A + 1/A)(1/q - 1) + 2)`，`A = 10^(gain_db/40)`。
  producer `fit` 默认 S=0.7；消费方若自己实现 biquad，必须用这个 S 公式，不得套 peaking 的 Q 公式
  （同参最多差 0.48 dB）。

## 3. 渲染映射（khaen-channelmap render → CamillaDSP config）

| profile peq | CamillaDSP filter parameters |
| --- | --- |
| peaking | `{"type":"Peaking","freq":f,"q":q,"gain":gain_db}` |
| low_shelf | `{"type":"Lowshelf","freq":f,"slope":12.0*q,"gain":gain_db}` |
| high_shelf | `{"type":"Highshelf","freq":f,"slope":12.0*q,"gain":gain_db}` |

CamillaDSP 的 `slope` 单位是 **dB/oct**，effective S = slope/12；所以 `slope=12*q` 与 producer 的 S
系数完全一致（CamillaDSP 源码 `biquad.rs` 的 Slope 分支）。shelf 必须写 `slope`，写 `q` 会走
CamillaDSP 的 Q 型 shelf，与 producer 模型最多差 0.48 dB（CONTRACT §4.2 要求用 Slope）。

## 4. shelf 的 q 上限就是 CamillaDSP 的 slope 上限

CamillaDSP 4.1.3 校验 `Slope must be <= 12.0`（`biquad.rs` check_slope）。映射是 `slope = 12*S`，
所以 **S 必须 ≤ 1**。裁决 5 把这条收成校验错误，而不是「validate 通过、render 再被 --check 打回」：

- shelf 的 `q=S` ∈ [0.1, 1.0]：validate/render rc=0，`slope = 12*q`（S=1 ↔ slope=12.0）。
- shelf 的 `q` < 0.1 或 > 1.0：rc=3（`kind=value`），不产出文件。q=1.01 与 q=2.0 都拒绝。
- peaking 的 q 仍是 [0.1, 10]；v1.1 契约允许 peaking q>10，但本渲染器不接受（走 CamillaDSP 片段那条路）。
- producer `fit` 默认 S=0.7，落在区间内。需要更陡的架子时应拆段，而不是把 S 抬过 1。

## 5. 示例（profile 片段）

    schema: khaen-channelmap/v2
    version: 2
    output:
      device: auto
      channels: 8
      layout: "4.1.2"
      roles:
      - id: FL
        outputs: [0]
        sources:
        - {source: FL, gain_db: 0.0}
        peq:
        # peaking（type 可省略）
        - {freq: 23.8, q: 6.0, gain_db: -6.0}
        # shelf：q 是 RBJ slope S；渲染为 slope = 12*q = 8.4 dB/oct
        - {type: low_shelf, freq: 100.0, q: 0.7, gain_db: -3.0}
        - {type: high_shelf, freq: 5000.0, q: 0.5, gain_db: 2.0}

CLI：

    khaen-channelmap validate profile.yml
    khaen-channelmap render profile.yml -o /run/khaen/camilladsp.yml --playback-device hw:motu,0

## 6. producer → profile 的写入规则（CONTRACT §4.1）

对每个校准通道，找到同 name 的 role：

    role.peq = render_role.peq + [{freq, q, gain_db, type?} for f in filters]

- peaking 可省略 type；`low_shelf`/`high_shelf` **不能丢 type**（丢 = 静默降级，禁止）。
- 按顺序追加；总数 ≤32。
- 不修改 input/remap/bass_management 等其它字段，不改 schema。

## 7. 回归与测试

`upmix/tests/test_channelmap_peq_type.py`（8 个用例，离线）：

    python3 upmix/tests/test_channelmap_peq_type.py -v

覆盖：peaking 缺省/显式 type、shelf 映射 slope=12q、未知 type rc=2 不产文件、peaking q 越界 rc=3、
shelf q=1.0 边界（slope=12.0）、shelf q=1.01 与 q=2.0 rc=3 且不产文件、freq/gain 越界。

回归证据（本机，2026-11-16）：

- 既有 peaking-only profile 与改动前（git HEAD 版本）的 `validate`/`print --json`/`render`
  输出逐字节相同（diff 为空）。
- 含 shelf 的渲染配置在 CamillaDSP 4.1.3 `--check` 下为 `Config is valid`（本机 Windows 构建无 ALSA，
  校验时把 devices 换成 Stdin/Stdout，filters/pipeline 校验路径不变；板端用真实 ALSA 配置复验）。
