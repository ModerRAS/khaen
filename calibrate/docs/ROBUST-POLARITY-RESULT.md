# khaen-calibrate 多遍扫频 / 鲁棒极性 / 时钟漂移

日期：2026-11-16。只改了 `projects/khaen-audio-dsp/calibrate/`。没有连板、没有动声卡。`khaen-calibration/v1` 的必填字段没有加。

## 改了哪些文件

| 文件 | 内容 |
| --- | --- |
| `src/dsp.rs` | 首达窗、低频/全带相关、相位投票、重复起点拟合、sinc 重采样、频响平均 |
| `src/robust.rs` | 多遍切段、两层投票、漂移补偿后再分析 |
| `src/main.rs` | `measure-multi` / `analyze` / `merge` 接上新参数；参考麦文件不进校准通道 |
| `src/raw.rs` | 两路 `arecord` 同时起再 `aplay`；CueMix `direct-mute` 的 HTTP 客户端 |
| `src/model.rs` | analysis JSON 的可选字段；校准里 SUB 可以不写 `polarity` |
| `README.md` | 新参数和投票规则 |
| `BOARD-T1-T5-RUNBOOK.md` | 板端推荐命令：`--repeats 3 --capture2 hw:4,0` |
| `examples/offline-robust/calibration.yml` | 合成夹具 merge 结果，校验器 0 error |

## CLI

不带 `--repeats`（默认 1）且不带 `--robust-polarity` 时，`polarity` 仍是 IR 峰符号。`--repeats` 大于 1，或显式 `--robust-polarity`，`polarity` 才改成投票结果。

主票是 `direct_window`、`lowband`（100–500 Hz）、`matched`。三者同号就是 `high`，`ir_peak` 和 `phase_vote` 都不能把这个结果否决成 `low`。主票不齐时才把 `phase_vote` 算进去，置信度最高到 `low`。一遍反号，跨遍结果就是 `low`。

SUB（名字以 `SUB` 开头，或扫频截止 ≤ 200 Hz）在听音位上 `polarity_confidence=unknown`，`merge` 不写 `polarity`，除非 `--allow-sub-polarity-from-listening-position` 或 `--sub-reference-nearfield`。

`--capture2` 与主采集同时录音。参考结果写 `REF-chXX.analysis.json`（`measurement_role=reference`，merge 会跳过），并写入主 JSON 的 `reference`。两边都有结论且符号不同时，置信度降为 `low`，极性跟参考麦。

`clock_ppm = (实测间隔 / 标称间隔 − 1) × 1e6`。超过 `--max-ppm`（默认 100），或相对标称栅格的残差大于 0.25×间隔，该遍 `onset_invalid=true`，不进延迟和 ppm 聚合。`--drift-correct` 用 32 点 Hann-sinc 把采集拉回标称时钟后再分析。

## 新旧判据

纯 IR，直达声在样本 40、幅度 −1.0，5 ms 后反射 +1.05（`reflection_five_ms_flips_peak_not_direct_window`）：

| 判据 | 符号 |
| --- | --- |
| 旧：`sign(IR[argmax\|IR\|])` | +1（被反射翻掉） |
| 新：首达后 2 ms 求和 | −1（停在直达声） |

Farina 通道：8 个 +0.55 的直达样本，5 ms 后一个 −1.05 的尖峰（`ir_peak_negative_lowband_positive_stays_high`）。测试锁定的结果是：

| 量 | 值 |
| --- | --- |
| 旧 `polarity`（IR 峰） | −1 |
| `direct_window` | +1 |
| `lowband` | +1 |
| 提交的 `robust_polarity` | +1 |
| `polarity_confidence` | high |

`high` 要求三张主票同号，所以这条通道的 `matched` 也是 +1。`phase_vote` 不参与这次否决。

## 合成夹具（`cargo test` 里的离线流程）

`tests::offline_repeats_reference_and_sub_merge` 造了 3 遍扫频：FL 放在槽起点后 20 个样本，参考麦再晚 15 个样本；SUB1 用 20–130 Hz。`measure-multi --capture-file --capture2-file --repeats 3` 的结果：

| 通道 | delay_samples | polarity | confidence | clock_ppm |
| --- | --- | --- | --- | --- |
| FL | 20.000 | +1 | high | 0.0 |
| FL 的参考麦 | 35.000 | +1 | high | 0.0 |
| SUB1 | 14（归一化后校准文件里是 6） | 0（不提交） | unknown | 0.0 |

FL 三遍的 `polarity_votes` 都是 +1，`reference_xcorr` 是 +1。merge 后的 `examples/offline-robust/calibration.yml`：FL `polarity: normal`，SUB1 没有 `polarity` 键。校验器：

```
OK    examples\offline-robust\calibration.yml
  WARN   channels[1].polarity 缺省（建议显式 normal）
结果：errors=0 warnings=1
```

同一份采集若直接跑 `analyze`（它不知道槽的名义起点，不会减掉 pre-roll）：

```
channel 0: repeats=3 delay=9620.000 samples polarity=+1 confidence=high clock_ppm=0.00
```

9620 = 0.2 s 前置静音（9600）+ 20 个样本。`measure-multi` 减去名义槽起点后剩下 20。

旧的 `examples/offline` 用新二进制重新 merge，极性仍是 +1，抵消检查 worst 9.03 dB，和原来的离线记录一致：

```
ch0: 0 stages rms=0.000dB max=0.01dB delay=0.000ms polarity=+1
ch1: 4 stages rms=0.882dB max=8.18dB delay=0.073ms polarity=+1
ch2: 0 stages rms=0.000dB max=0.00dB delay=0.208ms polarity=+1
ch3: 0 stages rms=0.000dB max=0.00dB delay=0.417ms polarity=+1
merged 4 channels -> target\offline-remerge.yml ; cancellation check PASS (worst 9.03 dB @ 19897.0 Hz)
```

时钟：`clock_ppm_within_2_and_drift_correct_delay` 对整段采集注入 20 ppm 和 50 ppm。估计误差 ≤ 2 ppm；`--drift-correct` 之后延迟误差 ≤ 0.5 sample。测试通过，中间值没有另存。

## 命令输出

工作目录 `projects/khaen-audio-dsp/calibrate`。release 二进制 `target/release/khaen-calibrate.exe`，1,927,168 字节。下面 5 条 warning 是原来就有的未使用项（`A.pos`、`ChannelAnalysis`、`mono_mix`、`run_checked`、`SweepSpec::new`），这次没有新增。

```
===== cargo fmt --check =====
EXIT 0

===== cargo check =====
    Checking khaen-calibrate v0.1.0 (C:\WorkSpace\Claw\projects\khaen-audio-dsp\calibrate)
warning: field `pos` is never read
  --> src\main.rs:27:5
   |
25 | struct A {
   |        - field in this struct
26 |     m: HashMap<String, String>,
27 |     pos: Vec<String>,
   |     ^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: struct `ChannelAnalysis` is never constructed
  --> src\model.rs:30:12
   |
30 | pub struct ChannelAnalysis {
   |            ^^^^^^^^^^^^^^^

warning: function `mono_mix` is never used
  --> src\raw.rs:56:8
   |
56 | pub fn mono_mix(channels: &[Vec<f64>]) -> Vec<f64> {
   |        ^^^^^^^^

warning: function `run_checked` is never used
  --> src\raw.rs:97:8
   |
97 | pub fn run_checked(mut cmd: Command, label: &str) -> Result<(), String> {
   |        ^^^^^^^^^^^

warning: associated function `new` is never used
  --> src\sweep.rs:13:12
   |
12 | impl SweepSpec {
   | -------------- associated function in this implementation
13 |     pub fn new(sample_rate: u32, seconds: f64, level_db: f64) -> Self {
   |            ^^^

warning: `khaen-calibrate` (bin "khaen-calibrate") generated 5 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.35s
EXIT 0

===== cargo test =====
   Compiling khaen-calibrate v0.1.0 (C:\WorkSpace\Claw\projects\khaen-audio-dsp\calibrate)
warning: field `pos` is never read
  --> src\main.rs:27:5
   |
25 | struct A {
   |        - field in this struct
26 |     m: HashMap<String, String>,
27 |     pos: Vec<String>,
   |     ^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: struct `ChannelAnalysis` is never constructed
  --> src\model.rs:30:12
   |
30 | pub struct ChannelAnalysis {
   |            ^^^^^^^^^^^^^^^

warning: function `mono_mix` is never used
  --> src\raw.rs:56:8
   |
56 | pub fn mono_mix(channels: &[Vec<f64>]) -> Vec<f64> {
   |        ^^^^^^^^

warning: function `run_checked` is never used
  --> src\raw.rs:97:8
   |
97 | pub fn run_checked(mut cmd: Command, label: &str) -> Result<(), String> {
   |        ^^^^^^^^^^^

warning: `khaen-calibrate` (bin "khaen-calibrate" test) generated 4 warnings
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.92s
     Running unittests src\main.rs (target\debug\deps\khaen_calibrate-2b1d8966bb6f561a.exe)

running 21 tests
test dsp::tests::reflection_five_ms_flips_peak_not_direct_window ... ok
test dsp::tests::clock_fit_drops_gross_outlier ... ok
test peq::tests::smooth_preserves_grid ... ok
test peq::tests::fit_synthetic_peaking ... ok
test dsp::tests::fractional_delay_is_finite ... ok
test sweep::tests::fades_are_zero_ish_at_edges ... ok
test sweep::tests::length_and_range ... ok
test tests::merge_polarity_sub_omitted_unless_allowed ... ok
test tests::merge_validation_detects_missing_delay_correction ... ok
test tests::negative_n1_polarity_detected ... ok
test tests::negative_n2_delay_detected ... ok
test tests::negative_n3_peq_detected ... ok
test tests::negative_n5_sub_delay_detected ... ok
test dsp::tests::ir_peak_negative_lowband_positive_stays_high ... ok
test robust::tests::sub_band_is_not_a_committed_sign ... ok
test dsp::tests::farina_recovers_delay_and_polarity ... ok
test robust::tests::repeated_identical_is_high_and_median_delay ... ok
test robust::tests::one_inverted_repeat_majority_low ... ok
test robust::tests::reference_and_main_delays ... ok
OK    C:\WorkSpace\Claw\projects\khaen-audio-dsp\calibrate\target\robust-fixture\calibration.yml
  WARN   channels[1].polarity 缺省（建议显式 normal）
结果：errors=0 warnings=1
test tests::offline_repeats_reference_and_sub_merge ... ok
test robust::tests::clock_ppm_within_2_and_drift_correct_delay ... ok

test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 28.54s

EXIT 0

===== cargo build --release =====
   Compiling khaen-calibrate v0.1.0 (C:\WorkSpace\Claw\projects\khaen-audio-dsp\calibrate)
warning: field `pos` is never read
  --> src\main.rs:27:5
   |
25 | struct A {
   |        - field in this struct
26 |     m: HashMap<String, String>,
27 |     pos: Vec<String>,
   |     ^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: struct `ChannelAnalysis` is never constructed
  --> src\model.rs:30:12
   |
30 | pub struct ChannelAnalysis {
   |            ^^^^^^^^^^^^^^^

warning: function `mono_mix` is never used
  --> src\raw.rs:56:8
   |
56 | pub fn mono_mix(channels: &[Vec<f64>]) -> Vec<f64> {
   |        ^^^^^^^^

warning: function `run_checked` is never used
  --> src\raw.rs:97:8
   |
97 | pub fn run_checked(mut cmd: Command, label: &str) -> Result<(), String> {
   |        ^^^^^^^^^^^

warning: associated function `new` is never used
  --> src\sweep.rs:13:12
   |
12 | impl SweepSpec {
   | -------------- associated function in this implementation
13 |     pub fn new(sample_rate: u32, seconds: f64, level_db: f64) -> Self {
   |            ^^^

warning: `khaen-calibrate` (bin "khaen-calibrate") generated 5 warnings
    Finished `release` profile [optimized] target(s) in 33.41s
EXIT 0
```

原先 11 个测试还在，新的 10 个是极性、重复、时钟、参考麦和 merge 规则。

## 限制

- 板端没有跑。双路 `arecord` 和 CueMix 静音只在代码里，本机用 `--capture-file` / `--capture2-file` 代替真设备。
- 单遍且不加 `--robust-polarity` 时，写进 `polarity` 的仍是 IR 峰。诊断字段里看得到投票，但 merge 仍按旧符号。
- `phase_vote` 在四个频点上会被 5 ms 左右的反射打成和主票相反的票。主票一致时它不降置信度。
- 听音位的 SUB 极性故意不提交。近场要加 `--sub-reference-nearfield`，工具不会自己判断麦的位置。
- 漂移补偿是固定 32 点 sinc。20/50 ppm 的合成用例过了；很大的 ppm 或起点已经锁错时应看 `clock_ppm_ok`，不要开 `--drift-correct`。
- `analyze` 给出的是文件里的绝对起点。`measure-multi` 在 `--repeats`>1 时改成相对该槽名义起点的超额延迟，这样跨通道不会把槽间距写进校准。

## 参考麦起点（2026-11-16 板端复算）

`scratch/lead/board-window/robust-t4` 的参考麦（MOTU ch0，Neumann MA 1）把第一遍锁在 lag 10,741，后两遍在 7,867。三遍一拟合，`clock_ppm=-5756`，`clock_ppm_ok=false`，但这个结果仍然覆盖了主麦极性。

根因有两层。低频隆隆让绝对相关在 10,741 大于真实扫频（约 7,861）。带通之后假峰的归一化相关仍然更高（大约 0.60 对 0.23），所以只改归一化选不出真峰。第二遍真峰旁边还有一个晚约 1,085 样本的旁瓣；拿它当锚点时，假峰残差从 2,873 降到 1,789，进了 2,000 样本的容差，假峰又因为相关值更大赢过真起点。

改法：

- 选峰前按扫频通带做带通。全带是 80–8000 Hz，并且不超过扫频高端的 0.9 倍；窄带/SUB 从 `start_hz`/`end_hz` 收进 90%，不写死 20–130。
- 提名用归一化相关。带通峰会偏早（UMIK 上大约 640 样本），再在 ±1,200 样本内收回全带 `|c|` 最大的位置。
- 栅格间距只用和标称间隔相差不超过 1000 ppm 的相邻差，否则用标称间隔。偏差取 `min(5% 间距, 2000 样本)`。锚点必须是自己那一格里相关最强的峰，旁瓣不能把假峰拉进来。
- 有效起点少于 3 个不估计 `clock_ppm`。参考麦 `clock_ppm_ok=false`、`onset_invalid=true`、`snr_db<20` 或极性 `unknown` 时写 `reference.valid=false`，不改主麦极性。槽间间隔少于 3 个时 `slot_clock_ppm` 为空。
- 双设备采集且没有显式 `--playback-offset-seconds` 时，aplay 前等待和名义偏移都是 1.0 s。这次复算用的是已经录好的 0.35 s 数据，没有重录。

新测试：`onset_band_full_sweep_and_sub`、`clock_fit_needs_three_points`、`loud_low_frequency_burst_does_not_steal_the_grid`、`grid_filter_rejects_peak_shifted_by_3000`、`shifted_repeat_is_dropped_and_two_points_do_not_estimate_ppm`、`sidelobe_cannot_pull_false_peak_onto_the_grid`、`invalid_reference_does_not_override_main_polarity`。

复算只切了第一槽搜索窗 `[76800, 885600)`（名义起点 88,800，窗前 0.25 s）。`analyze` 打印的延迟相对窗起点；和旧 JSON 的超额延迟对比时减去 12,000。

| 量 | 修复前 | 修复后 |
| --- | --- | --- |
| REF 三遍延迟（窗内） | 10,740.76 / 7,867.15 / 7,867.14 | 7,860.73 / 7,863.94 / 7,860.73 |
| REF 超额延迟 | −4,132.85 | −4,139.27 |
| REF clock_ppm | −5,756.46，`ok=false`，三遍 `onset_invalid` | 估计值 +12.84 ppm，与不确定度同量级，不可当物理漂移；三遍起点已落在同一栅格（延迟散差 ≤3.2 样本），且假峰 10,741 被栅格剔除——这是本次栅格修复的验收依据 |
| REF 极性 | +1，low（无效结果仍写出） | +1，low |
| UMIK 延迟（窗内） | 11,514.90 | 11,514.90 |
| UMIK 超额延迟 | −485.10 | −485.10 |
| UMIK clock_ppm | −1.86，`ok=true` | −1.86，`ok=true` |
| UMIK 极性 | 听音位 −1；被无效参考覆盖成 +1 | −1，low |

第一遍的脉冲峰仍比后两遍早约 2,874 样本（索引 237,119 对 239,993），这一票是 −1，所以参考极性置信度保持 low。起点本身已经落在后两遍的栅格上，没有再把 ppm 拉到 −5756。假峰 10,741 被栅格丢掉。

这次参考结果有效，且和主麦 −1 不一致。按规则，主麦会跟随参考麦并把置信度降为 low。上次覆盖是无效参考造成的；这次若覆盖，依据是三遍栅格已经对齐。板端复测由 Lead 做，本机没有连板。

`cargo fmt --check`、`cargo check`、`cargo test`（43 通过）、`cargo build --release` 都过了。

---

## 2026-11-16 更正：REF +12.84 ppm 的解释与 snap 根因

上表“修复后 REF clock_ppm=+12.84, ok=true”**不是**“漂移验证通过”，更**不能**当物理漂移证据：

- 3 遍 × 5 s 的跨度只有 10.4 s，1 样本 ≈ 2 ppm；+12.84 ppm ≈ 6.4 样本的跨度误差。它来自单遍起点反射干扰（robust-t4 REF repeat0 的 IR 峰偏 2,874 样本 = 60 ms 反射），旧 `fit_clock` 的残差门限（0.25×interval）从不 reject，回归把这一点放大成“ppm”。
- 正确口径：`clock_ppm` 必须与 `clock_ppm_uncertainty_ppm` 一起看；`clock_ppm_significant = |ppm| > 2σ` 才允许 `--drift-correct`。REF 这种量级（ppm 与 σ 同量级）一律标 not significant、不重采样、不当漂移证据。
- 本次栅格修复的验收依据是：**三遍起点落在同一栅格（延迟散差 ≤3.2 样本）+ 假峰 10,741 被剔除**，不是 ppm 数值。

### v1.1 根因修复（2026-11-16，详见 docs/P0-FIX-2026-11-16.md）

- 删除了 repeat 路径的“带通峰 → 未归一化全带 |c| ±1200 snap”。全带 |c| 被 20–300 Hz 宽主瓣主导，argmax 可把起点拉晚数百样本：FR 事故 +706、FL 三遍 +639..670（伪 −62.4 ppm）。带通归一化峰 + 抛物线（与单遍 `bandpassed_delay` 同源）已足够。
- `fit_clock` 换成稳健版：相邻差中位数、|d−I|>500 拒、≥2 accepted 才估 ppm、accepted 端点中位数当锚、逐遍残差>500 只标该遍 invalid、2 遍无 hint 时标 insufficient；新增 uncertainty/significant、suspect mask（跨遍 IR 峰 |Δ|>200 标 suspect）、`--drift-correct` 只在 significant 时启用。
- 旧的 `snap_correlation_lag` 保留为参考实现（`#[allow(dead_code)]`），不再被 repeat 路径调用。测试名相应更新：`clock_fit_needs_two_accepted_diffs`、`clock_fit_marks_only_bad_first_pass`、`clock_fit_anchor_hint_resolves_two_pass_one_bad`、`first_repeat_shifted_706_is_marked_invalid_only` 等。

