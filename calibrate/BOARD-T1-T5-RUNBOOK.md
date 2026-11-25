# 板端 T1–T6 校准验收 Runbook（v15，B2+B3 修订 2026-11-17）

> 依据：`calibration-spec/ACCEPTANCE.md`（2026-11-17 B2 修订，K4/K2/K7）与 `calibration-spec/CONTRACT.md`。
> 变更（C-15/16/17/18 + B2 P1）：T4 改为 4.1.2 真实分频 pair（FL/FR:SUB1:80）、5.3.2 标 NEEDS_HW；T1 固定 −12 dBFS 并记录 kMainTrim/噪声底；
> T3 保留 + 新增 T6 holdout；话筒 90° 校准/朝天花板；merge 的 negative-controls 由“可省略”改为“省略即 INCOMPLETE”。
> **B3 修订（2026-11-17）**：C-19 capture 映射按 K7（ch12=L/ch13=R、ch8/9=内部 USB loopback，删旧 S/PDIF）；T5 时长统一写秒（≥3600 s，10 min 仅 screening）；T1/T4 的 capture 通道数按 dump-hw-params 实测（当前 MOTU=20ch）。
> **裁决落地（2026-11-17，D8/D3/D7）**：D8 SUB1 唯一物理口=**Line Out 8（槽位7）**，Line Out 10（槽9）仅 2026-11 临时测试历史、不是候选；D3 5.3.2 中置按设计=**全频（与主箱相同）**，当前无中置，中置 HP80/低频重定向/中置 pair/测试全部 **deferred/NEEDS_HW**，移出 P1（4.1.2 不受影响）；D7 **允许 +3 dB boost**，但须执行削波/headroom/总增益帽检查并写入 T3/T6 报告（详见 §0.1）。
> 前置：soak 结束/停止（`sudo systemctl stop tspi-soak12.service`），MOTU=card4、UMIK=card3；一次只做一项；不接功放/后级（T5 除外）；不连外网。

## 0. 每次测量前统一设置（C-16）

1. CueMix/MOTU 主输出 trim（`kMainTrim`）**置 0**；无法置 0 时记录实际值并写明补偿方式（−28 dB 会把信号整体压低 28 dB，测到的是噪声底）。
2. 记录 `drive_level_dbfs`、`k_main_trim_db`、`noise_floor_dbfs`（无激励时的本机采集底）；三者缺失 → 该项 INCOMPLETE。
3. UMIK-1：**用 90° 校准文件、麦轴垂直朝天花板**（听音位多声道；0° 只用于单只近场）。记录 `mic{model,serial,cal_file,sha256,orientation}` 与麦坐标/麦高。
4. 电平：T1 电学环回固定 **−12 dBFS**；T2/T4 默认 −30 dBFS，带限 SNR<25 dB 时升到 −12 dBFS 重测（不削波）。

## 0.1 裁决口径（D8/D3/D7，2026-11-17）

1. **D8（SUB 物理口）**：4.1.2 的 SUB1 唯一物理口 = **Line Out 8（槽位7，0-based）**。T4 命令保持 `--channels 0,1,2,3,4,5,7`、`--pairs FL:SUB1:80,FR:SUB1:80`，不再设“out8 不出声→改用 ch9/out10”的备选分支。Line Out 10（槽9）仅作为 2026-11 out8 静音问题的临时测试历史注记，不是设计候选；若发现实际接线与 Line Out 8 不符，先停测并回报，不得静默切换。
2. **D3（5.3.2 中置）**：中置按设计=**全频、与主箱相同**（不设 HP80、不做低频重定向）。当前无中置单元，因此中置 HP80/重定向、中置 pair 与中置相关测试全部 **deferred/NEEDS_HW**，移出 P1 验收；5.3.2 T4 只按 FL/FR:SUB1:80 pair + SUB 组检查，C 仅单通道记录/不参与 pair。4.1.2 布局与 pair 完全不受影响。
3. **D7（boost）**：允许 **+3 dB boost**（填谷），必须同时满足 ACCEPTANCE §5 boost/headroom 行：单段 `filters[].gain_db>0` ≤ **+3.0 dB**；每通道正向总和 >6 dB 记 warning、>12 dB 拒绝；**预测最大累计响应峰值 ≤ −6 dBFS（headroom ≥6 dB）**；实测扫频/节目峰值 ≤ **−1 dBFS**、无数字削波。T3/T6 报告记录 `boost_filters/total_boost_db/max_predicted_peak_dbfs/headroom_db/measured_peak_dbfs`；不满足则减少 boost，不得用削波换平直。

## 0.1 部署
```bash
scp target/release/khaen-calibrate lckfb@192.0.2.215:/tmp/ && ssh lckfb@192.0.2.215 'chmod +x /tmp/khaen-calibrate'
sudo mkdir -p /var/lib/tspi-calib/{t1,t2,t4,t6,t5}
```

## T1 前置：CueMix 状态确认（用户当前：仅 in7→out7、in8→out8）

用户已确认：CueMix 当前只有 **in7→out7 ON、in8→out8 ON**，9/10 未做直通；out9→in7 不会成环，T1 可直跑。但 T1 时必须：
1. **out7/out8 不接后级**（不接功放/有源箱/耳机），避免 in7 信号从 out7 出声或反馈；
2. 记录 CueMix 状态（截图 + 文本）：
   - `cuemix_in7_to_out7: on`
   - `cuemix_in8_to_out8: on`
   - `cuemix_in7_to_out9: none`
   - `cuemix_other_input_to_output: none`
   - `cuemix_out7_out8_downstream: disconnected`
   - 截图 `/var/lib/tspi-calib/t1/cuemix-routing.png`
3. T1 报告字段（ACCEPTANCE §3.1，必须含新增字段）：
   ```
   t1.status: partial
   loopback_map_1based: [{play: 9, capture: 7}]
   covered_outputs: [9]
   covered_inputs: [7]
   uncovered_outputs_1based_4_1_2_22ch: [1..8, 10..22]
   drive_level_dbfs: -12
   k_main_trim_db: 0
   noise_floor_dbfs: <实测>
   isolation: other capture channels <= -60 dB AND >=10 dB below noise floor
   mitigation: remaining outputs via file backend + T4 UMIK acoustic evidence
   claim: non-1:1 spot check only; does not claim 1:1 mapping
   cuemix_routing: {in7_to_out7: on, in8_to_out8: on, in7_to_out9: none, other_input_to_output: none, out7_out8_downstream: disconnected, screenshot: /var/lib/tspi-calib/t1/cuemix-routing.png}
   final_insert_direct_routes: ["in7->out7", "in8->out8"]   # 最终 insert 前必须全部关闭
   ```
4. **最终 insert 模式必须关闭 in7→out7、in8→out8 以及全部 input→output 直通**，否则硬件直通与 USB DSP 双路径并存；报告需写明该前提。

## T1：MOTU 非 1:1 电学环回（**partial 抽检：out9 → in7**）

物理：MOTU **out9 → in7**（1 根线；1-based）。out9 为 10 路 TRS **线路输出**之一（不是耳机）；in7 为**线路输入**（不是话放输入，无 48V）。
**固定 −12 dBFS 驱动**（电学路径不削波；-30 dBFS 在 −28 dB trim 下会被噪声底淹没，2026-11 实测只有 −53 dB）。
仍建议先在 MOTU/Cuemix 路由里确认 out9=物理输出 9、in7=物理输入 7，且无 +48V/高增益。

```bash
arecord -D hw:4,0 --dump-hw-params /dev/null   # 按实测填 capture 通道数（当前 MOTU=20ch）
# 先测无激励噪声底（3 s 静音），写进报告
/tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t1 --out-prefix /var/lib/tspi-calib/t1/t1-noise \
  --loopback-map 9:7 --names NOISE \
  --playback hw:4,0 --playback-channels 22 --capture hw:4,0 --capture-channels 20 \
  --rate 48000 --seconds 3 --level-db -120 --pre-seconds 0.5 --post-seconds 0.5 \
  --period-size 128 --buffer-size 512
/tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t1 --out-prefix /var/lib/tspi-calib/t1/t1 \
  --loopback-map 9:7 --names OUT9_IN7 \
  --playback hw:4,0 --playback-channels 22 --capture hw:4,0 --capture-channels 20 \
  --rate 48000 --seconds 5 --level-db -12 --pre-seconds 1.5 --post-seconds 1.5 \
  --period-size 128 --buffer-size 512 --verify-mapping
```

判读（**T1-partial，只覆盖 out9→in7**）：
- 映射：播放 out9 的最大能量必须出现在采集 in7；出现在别的采集通道直接 FAIL；
- **隔离（新口径）**：其他采集通道（in1..in6、in8）相对 in7 ≤ −60 dB，**且比本机采集噪声底低 ≥10 dB**；
  只满足前者不满足后者 = 测到的是噪声底，该项 INCOMPLETE（重测/提高电平）。
- 延迟/极性：链路抽检值；CamillaDSP 注入 0/1/5/10 ms 各 5 次（冻结测试配置 + render_offsets，配置 sha256 入报告），
  误差 ≤0.5 sample、重复性标准差 ≤0.2 sample；工具暂无 `--inject-*`，不得口述注入值。
- 极性链：若已知正相环回测到 invert，后续声学测量统一加 `--polarity-offset invert`（只覆盖 MOTU DAC/ADC 段，不代表声学链绝对极性）。
- **范围**：只声称 out9→in7 的映射/隔离/极性/延迟链路；**不声称 out7→in7 或 1:1 映射**；其余通道用文件后端 + T4 UMIK 声学旁证；
- 报告必须写 `T1: partial (out9->in7 only); 1:1 mapping and other channels not verified`。

## T2：UMIK-1 重复性/漂移/SNR（48k）

```bash
# kMainTrim=0；麦 90° 校准、麦轴朝天花板；记录 cal 文件 sha256
for i in 1 2 3; do
  /tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t2/run$i --out-prefix /var/lib/tspi-calib/t2/run$i \
    --channels 0,1 --names FL,FR --playback hw:4,0 --playback-channels 22 \
    --capture hw:3,0 --capture-channels 1 --rate 48000 --seconds 5 --level-db -30 \
    --pre-seconds 1.5 --post-seconds 1.5 --period-size 128 --buffer-size 512 --mic umik
done
# 判读：3 次 delay 极差 ≤0.3 sample；极性一致；100Hz–10k 1/12oct 极差 ≤0.4dB；SNR(200–8k)≥25dB；峰值≤-1dBFS
# 漂移：报告 clock_ppm + clock_ppm_uncertainty_ppm；只有 |ppm|>2σ 才算漂移；N=1 不可估计、不得 --drift-correct
```
T2 数值阈值是 engineering default、待 pilot（先用现有 3 次 + 加测 5–10 次统计极差/σ）。

## T3：应用校准后逐通道复测（必须，K4）

1. 把 `calibration.yml` 应用到目标 profile/CamillaDSP 配置；保留 apply diff 与 sha256；
2. `tspi-channelmap validate/render` rc=0；独立配置 `camilladsp --check` rc=0；
3. 按 T2 同条件（同麦同位置同电平）复测 T3 各通道，按 ACCEPTANCE §5 表判定（残余延迟 ≤0.5 sample、极性与 YAML 一致、
   PEQ 实现差 ≤0.75 dB、目标偏差 ≤±3 dB 仅统计可校正频段、无削波）；
4. **禁止用 merge 的 predicted 结果代替 T3**；T3 未执行 → INCOMPLETE。
5. **D7 boost/headroom 复核**：记录 `boost_filters`（freq/q/gain）、`total_boost_db`（每通道正向和）、`max_predicted_peak_dbfs`、`headroom_db`、`measured_peak_dbfs`；须满足 ACCEPTANCE §5（单段≤+3 dB、正向和>6 dB warning/>12 dB 拒绝、预测峰值≤−6 dBFS、实测≤−1 dBFS）。

## T4：真实布局逐通道 + pair/report

> **pair 口径（C-15）**：pair = 同一源信号 + 互补分频（LR4）两通道；fc 必须等于 profile 某 HP/LP 频率。
> effect channel（高度/环绕/中置）不参与 pair，改单通道验收。当前 4.1.2 只有 **FL:SUB1:80、FR:SUB1:80** 是真分频对。

### T4-A 当前布局 4.1.2（可执行；K1：FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，ch6 空）
```bash
arecord -D hw:3,0 --dump-hw-params /dev/null   # UMIK 常见 1ch；2ch 再用 2
# 4.1.2 播放通道 = 0,1,2,3,4,5,7（7 路角色；22ch 设备域）
/tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t4 --out-prefix /var/lib/tspi-calib/t4/t4 \
  --channels 0,1,2,3,4,5,7 --names FL,FR,SL,SR,HFL,HFR,SUB1 \
  --playback hw:4,0 --playback-channels 22 --capture hw:3,0 --capture-channels 1 \
  --rate 48000 --seconds 5 --level-db -30 --pre-seconds 1.5 --post-seconds 1.5 \
  --period-size 128 --buffer-size 512 --mic umik --polarity-offset normal
# 合并；分频点/对必须与 profile 一致（FL/FR HP80 + SUB1 LP80）
/tmp/khaen-calibrate merge --dir /var/lib/tspi-calib/t4 --out /var/lib/tspi-calib/t4/calibration.yml \
  --names FL,FR,SL,SR,HFL,HFR,SUB1 --layout 4.1.2 --crossovers 80 \
  --pairs FL:SUB1:80,FR:SUB1:80 --chunksize 128 --boost-cap-db 3 \
  --negative-controls /var/lib/tspi-calib/t4/negative-controls.json
```
- **负向用例 5/5**（N1–N5，N2=0.25 ms/12 samples）：把注入与检出结果写成 JSON 数组交给 `--negative-controls`；
  **省略 = not_run = T4 INCOMPLETE**（C-P1-6；工具当前不会因此改 overall，验收按 ACCEPTANCE §9 判）。
- **SUB 物理口（D8 已裁决）**：SUB1 唯一物理口 = **Line Out 8（槽位7）**；命令固定 `--channels 0,1,2,3,4,5,7`、`--pairs FL:SUB1:80,FR:SUB1:80`，不再有 ch9/out10 备选分支。**Line Out 10（槽9）仅 2026-11 out8 静音问题的临时测试历史注记，不是设计候选**；若实际接线不符先停测回报，不得静默切换。
- **D7 boost/headroom 报告**：记录 `boost_filters/total_boost_db/max_predicted_peak_dbfs/headroom_db/measured_peak_dbfs`；单段≤+3 dB、正向和>6 dB warning/>12 dB 拒绝、预测峰值≤−6 dBFS、实测≤−1 dBFS。
- 高度/环绕**单通道**验收：极性 convention、T3 残余延迟、文件后端映射证据；**不得**写 `FL:HFL:2000`/`FR:HFR:2000` pair（K2 后高度全频、无 2 kHz 分频）。
- 报告：pair_validation 为 **predicted**（direct 测量 + 模型），应用后必须再用 T3 复核；`global_validation` 只作 warning，
  不得据此否决 pair（C-P1-5；工具当前仍可能计入 overall，验收时以 ACCEPTANCE 复核并在报告注明）。

### T4-B 5.3.2（**NEEDS_HW**：十只单元/三 SUB 接好后才验收）
```bash
# FL0 FR1 C2 SL3 SR4 SUB1=5 SUB2=6 SUB3=7 HFL8 HFR9
/tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t4-532 --out-prefix /var/lib/tspi-calib/t4-532/t4 \
  --channels 0,1,2,3,4,5,6,7,8,9 --names FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR \
  --playback hw:4,0 --playback-channels 22 --capture hw:3,0 --capture-channels 1 \
  --rate 48000 --seconds 5 --level-db -30 --pre-seconds 1.5 --post-seconds 1.5 \
  --period-size 128 --buffer-size 512 --mic umik --polarity-offset normal
# 三 SUB 先做 §6.2 组检查，再用“SUB 组等效”与 FL/FR 成对；中置按 D3 全频设计、当前无中置，C 不参与 pair
/tmp/khaen-calibrate merge --dir /var/lib/tspi-calib/t4-532 --out /var/lib/tspi-calib/t4-532/calibration.yml \
  --names FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR --layout 5.3.2 --crossovers 80 \
  --pairs FL:SUB1:80,FR:SUB1:80 --chunksize 128 --boost-cap-db 3
```
- **D3**：5.3.2 中置按设计=**全频（与主箱相同）**，不设 HP80、不做低频重定向；当前无中置单元，中置 HP80/重定向、中置 pair 与相关测试全部 **deferred/NEEDS_HW**，移出 P1。
- 因此 5.3.2 验收只用 `FL:SUB1:80`/`FR:SUB1:80` + SUB 组检查；C 仅单通道记录/不参与 pair，SL/SR 同样不参与 pair（差信号且低频不汇总 SUB）。4.1.2 不受影响。

## T6：holdout 验证（K4，必须）

```bash
# 在甜点附近取 1–3 个 holdout 点（如 ±0.3 m 左右/前后，或不同麦高），这些点不参与 merge 拟合
# 应用校准后，按 T3 流程在每个 holdout 点重测（同电平同麦方向）
/tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t6/holdout1 --out-prefix /var/lib/tspi-calib/t6/holdout1 \
  --channels 0,1,2,3,4,5,7 --names FL,FR,SL,SR,HFL,HFR,SUB1 \
  --playback hw:4,0 --playback-channels 22 --capture hw:3,0 --capture-channels 1 \
  --rate 48000 --seconds 5 --level-db -30 --pre-seconds 1.5 --post-seconds 1.5 \
  --period-size 128 --buffer-size 512 --mic umik
```
- 报告 `holdout: {n_points, positions, design_points, residual_mean_db, residual_max_db, passed}`；
- holdout 未报 → 总体验收 INCOMPLETE（§0.6/§9）；伪 holdout（与设计点同位置）无效。
- **D7**：T6 每个 holdout 点同样记录 `boost_filters/total_boost_db/max_predicted_peak_dbfs/headroom_db/measured_peak_dbfs`；boost 配置在 T3/T6 必须一致（应用后同一文件）。

## 听音位极性（推荐，多遍 + 参考麦）

主麦 UMIK 放听音位（90° 校准、朝天花板），参考麦 MA1 进 MOTU（与播放同一时钟）。极性以参考麦的多估计投票为准，主麦用来交叉验证；
两边不一致时 `polarity_confidence=low`，两份结果都留在 JSON 里。SUB 不要用听音位符号，除非麦贴在低音炮上并加 `--sub-reference-nearfield`。

```bash
/tmp/khaen-calibrate measure-multi --out-dir /var/lib/tspi-calib/t4r --out-prefix /var/lib/tspi-calib/t4r/t4 \
  --channels 0,1,2,3,4,5,7 --names FL,FR,SL,SR,HFL,HFR,SUB1 \
  --playback hw:4,0 --playback-channels 22 \
  --capture hw:3,0 --capture-channels 1 \
  --capture2 hw:4,0 --capture2-channels 20 --capture2-channel 0 --capture2-name REF \
  --repeats 3 --repeat-gap 0.2 --robust-polarity \
  --rate 48000 --seconds 5 --level-db -30 --pre-seconds 1.5 --post-seconds 1.5 \
  --period-size 512 --buffer-size 2048 --mic umik \
  --band-limit 7:130 --max-ppm 100
```

`--repeats 3` 已经会改用投票结果；`--robust-polarity` 写上是为了意图明显。不要默认加 `--drift-correct`，先看 `clock_ppm` / `clock_ppm_ok` /
`clock_ppm_uncertainty_ppm`。`onset_invalid=true`、`|clock_ppm|` 大于 `--max-ppm` 或 **|ppm| ≤ 2σ** 时都不是真漂移（后者只是估计噪声）。

## T5：64/128 帧实时（0 xrun）

```bash
# 64/128 各跑 ≥3600 s（60 min；10 min 只作 screening，不写 PASS）；记录 period/buffer/xrun/RT 线程
# soak12.sh 的参数单位=小时：1 = 3600 s（ACCEPTANCE §7 已定稿 ≥60 min）
sudo sed -i 's/chunksize: 128/chunksize: 64/' /var/lib/tspi-soak/camilla.yml
sudo systemd-run --unit=tspi-t5-64 --property=Type=simple /bin/bash /var/lib/tspi-soak/soak12.sh 1
# 读 /var/lib/tspi-soak/status.csv 与 camilla.log；无 xrun、FF/80 CPU4-7
# CPU 口径：camilladsp 进程 utime+stime / 墙钟；同绑核同频率；记录空载本底；前后变化 ≤10%
```

## 交付给 upmix-dev
打包 `t1/t2/t3/t4/t6` 的 `*.fr.csv / *.analysis.json / *.diag.json / *.pairs.json / *.report.json`、
`negative-controls.json`、holdout 报告、mic cal 文件 sha256、camilla.log、status.csv、dmesg；按 ACCEPTANCE T0–T6 复算。
## 最终 insert 模式检查点（T5 后、正式替代 CueMix 前）

1. 关闭 CueMix **全部 input→output 直通**，明确包括 in7→out7、in8→out8；
2. 仅保留 USB playback→output；
3. 截图留证 `/var/lib/tspi-calib/final-insert/cuemix-final.png`；
4. 报告记录：`final_insert_direct_routes: []`、`cuemix_input_to_output: none`、`cuemix_usb_playback_to_output: enabled`；
5. 未关闭直通前不得声称“可替代 CueMix/insert 模式验收通过”。

## 已知工具缺口（不阻塞测量，验收时按 ACCEPTANCE 复核）
- `global_validation` 仍可能进入 `overall_status`（C-P1-5 待 audio-stack 改）；
- `negative_controls` 不参与 `overall_pass`（C-P1-6）——验收以 ACCEPTANCE §9 为准；
- 无 `capture_peak_dbfs` 硬门 / 带限 SNR 门（C-P1-11）——报告需补测并记录；
- 无 `--inject-delay-samples/--inject-peq`（C-P1-10）——T1 用冻结 CamillaDSP 测试配置；
- 报告缺 `tests[]`/holdout/mic 字段（C-P1-6/C-18）——T3/T6 由人工报告补齐。
