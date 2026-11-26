# 单点闭环复测：协议与现场手册

> 状态：离线链路已实现并在 Windows 上测试通过；**真机测量、下发、复测全部 NOT_RUN**（需要 MOTU 的 ALSA PCM 设备名与 08:30–12:00 / 14:30–18:00 出声窗口许可）。本文把窗口内要做的每一步写成可执行命令，并把判据、证据与回滚固定下来。

## 1. 目标与判据

目标 ④：单点校准产物在真实设备上闭环复测通过。三条硬判据：

1. **预测 vs 实测 ≤ 2 dB**：逐 1/3 倍频程带 |实测 − 预测| ≤ 2.0 dB（含等于），可比较带数不少于 3；
2. **可复现 sha**：方案、两段录音、四份产物都有 sha256，且同输入重跑报告逐字节相同；
3. **可回滚**：方案经两阶段 prepare/commit 下发，失败可 rollback 回上一版（控制面有界历史 16 版）；rt-host 场景回滚 = 换回旧方案文件并重启进程。

三态结论（缺数据绝不冒充通过）：

| 结论 | 含义 | 进程退出码 |
| --- | --- | --- |
| PASS | 每个可比较带都在 2 dB 内，且可比较带数 ≥ 3 | 0 |
| FAIL | 至少一个可比较带超容差 | 3 |
| INCOMPLETE | 可比较带不足（缺数据/录音太短/参考静音） | 4 |

## 2. 证据包

一次复测产出一个目录（必须为空或不存在），四份确定性文件：

| 文件 | 内容 |
| --- | --- |
| `predicted.json` | 由方案推出的该 lane 预测带域电平（相对频响，0 dB = 无滤波） |
| `observed.json` | 两段录音逐带相除的实测相对电平（缺数据记 null） |
| `report.json` | 三态判定 + 逐带 predicted/observed/delta/within_tolerance |
| `manifest.json` | 方案与两段 WAV 的 sha256、三份输出 sha256、判定与配置（无时间戳，可复现） |

清单 schema：`khaen-closed-loop-run-manifest/v1`；报告 schema：`khaen-closed-loop-report/v1`；电平集合 schema：`khaen-closed-loop-levels/v1`。字段细节见 `docs/CONTRACTS.md` 的 D 节。

## 3. 八个离线命令行工具

| 工具 | 作用 | 退出码 |
| --- | --- | --- |
| `khaen-single-point` | 合成场景 → 求解 → 报告/指标/FIR CSV/清单 | 0 成功 / 3 PARTIAL / 4 NONE / 2 输入 / 1 运行 |
| `khaen-fir-plan` | 手动指定 FIR CSV → 方案文件（crossover 承载 FIR） | 0 / 2 输入 / 1 写失败 |
| `khaen-scene-plan` | mimo 报告 + FIR 目录 → 方案文件（按报告选定变体，source 顺序即 lane 顺序） | 0 / 2 输入 / 1 写失败 |
| `khaen-plan-levels` | 方案文件 → 预测电平（predicted 侧） | 0 / 2 输入 / 1 写失败 |
| `khaen-band-levels` | 两段 WAV → 实测相对电平（observed 侧） | 0 / 2 输入 / 1 写失败 |
| `khaen-closed-loop` | 两份电平集合 → 三态报告 | 0 PASS / 3 FAIL / 4 INCOMPLETE / 2 输入 / 1 写失败 |
| `khaen-closed-loop-run` | 方案 + 两段 WAV → 四文件证据包 | 同上三态 + 2 输入 + 1 写失败 |
| `khaen-sweep` | 生成确定性测量扫频 WAV（多段、可加低频专用段） | 0 成功 / 2 输入 / 1 写失败 |

`deploy/closed-loop-verdict.sh` 是设备侧薄包装：检查文件与输出目录后调用 `khaen-closed-loop-run`，退出码透传。语法经 `bash -n` 检查，真机行为 NOT_RUN。

## 4. 现场操作序列（窗口内执行；每步标注状态）

前置条件（未满足就停）：

- 用户明确许可出声窗口；当前时段在 08:30–12:00 或 14:30–18:00 内；
- 已知 MOTU 的 ALSA PCM 设备名（只读 `aplay -l` 获取；2026-11-26 只读确认 card id 为 `UltraLitemk5`）；
- 已安装发布包：`/opt/khaen/current/bin/` 下有 `khaen-daemon`、`khaen-rt-host` 与七个校准命令行工具（**2026-11-26 只读检查：设备上尚未部署 khaen，只有 v1 `/opt/tspi-rt-stack`**，需先安装 khaen release）；
- 没有旧进程占用 MOTU PCM（**2026-11-26 07:18 只读检查：v1 `prod_bridge.py` + `aplay -D motulow16` 正占用 `hw:CARD=UltraLitemk5`**，停止它们需要用户明确许可）；
- 状态目录 `/var/lib/khaen/` 可写。

### 4.0 设备现状快照（2026-11-26 07:18，只读）

| 项 | 实测值 |
| --- | --- |
| 主机 | AoipDSP / 192.168.63.222 / Linux 6.12.117+deb13-rt-amd64 |
| 声卡 | card0 `UltraLitemk5`（MOTU UltraLite-mk5，USB 高速）；card1 `U18dB`（miniDSP Umik-1，测量麦）；card2 `HID`；card3 `PCH`；card4 `RAVENNA`（Merging） |
| MOTU PCM | `hw:CARD=UltraLitemk5,DEV=0`，S24_3LE / 22 声道 / 48000 Hz / period 6000 / buffer 24000；采样时刻 **RUNNING** |
| 当前占用 | v1 `prod_bridge.py`（PID 932650）+ `aplay -D motulow16 -f S24_3LE -c 16 -r 48000`（PID 932654）+ `arecord -D plughw:RAVENNA`（PID 932657）；已连续运行约 7.5 小时 |
| 旧栈 | `/opt/tspi-rt-stack/current -> releases/x86-rt-20261120`，含 v1 `tspi-rt-daemon`/`tspi-rt-host`/`tspi-calibrate`/`tspi-audio-web`；`tspi-rt-daemon.service` active |
| khaen 部署 | `/opt/khaen`、`/var/lib/khaen` **均不存在**（尚未部署） |
| 工具 | `/usr/bin/aplay`、`/usr/bin/arecord`、`python3` 3.13.5 可用 |

这张表只证明只读事实；它不证明 khaen 能出声，也不证明 MOTU 在 khaen 下的时序。设备窗口第一步仍要重新跑 4.1 并记录当次结果。

### 4.1 只读枚举与设备健康（状态：NOT_RUN）

```bash
aplay -l                        # 只读枚举：记录 MOTU 的 card 号、card id 与 PCM 名
aplay -L | head -40             # 只读：确认有哪些 PCM（别名不一定含 motu）
cat /proc/asound/cards          # 只读：card id 必须以此为准，不要凭记忆猜
cat /proc/asound/card0/pcm0p/sub0/status   # 只读：确认是否被占用（RUNNING 时先问用户能否停）
```

只允许一次只读诊断；不写 CueMix、不改 ALSA 音量、不做批量设备写。

### 4.2 30 分钟运行基线（状态：NOT_RUN）

用恒等方案或旧方案启动实时主机，落状态文件，观察是否丢块/爆音：

```bash
export KHAEN_ALSA_DEVICE=hw:CARD=UltraLitemk5,DEV=0   # 以第 1 步实测 card id 为准
# v1 审查记录里 N150 的 card id 是 UltraLitemk5（不是 MOTU）；照抄名字打不开设备。
export KHAEN_SAMPLE_RATE=48000
export KHAEN_RUN_SECONDS=1800
export KHAEN_STATUS_FILE=/var/lib/khaen/rt-host-status.json
export KHAEN_STATS_EVERY_BLOCKS=4000
/opt/khaen/current/bin/khaen-rt-host > /var/lib/khaen/rt-host-30min.log 2>&1
```

启动瞬间 RTP 包还没到时会出现短暂输入欠载，可能触发安全静音；`khaen-rt-host` 在输入恢复后
会自动清掉**仅限输入欠载**这一类安全故障并打印 `cleared recovered input-underrun fault`。
处理器/后端/换图故障不会自动清除，仍要求人工确认（fail-closed）。

启动时注意采样率：`khaen-rt-host` 会打印 `device rate before open`，打开 PCM 后再回读
`/proc/asound/.../hw_params` 并打印 `device rate after open`；若回读速率与方案速率不一致
（典型：MOTU 被外部设备改到 192 kHz），进程以退出码 1 结束并报 `device_rate_mismatch`。
处理：先让设备回到方案速率（v1 的 48 kHz keeper 或重新插拔 USB），再重启 rt-host。

判据：日志无 `run_block failed`、状态文件里 `output_errors` 为 0、无 xrun 告警。

### 4.3 8 小时浸泡（状态：NOT_RUN）

同 4.2 参数，`KHAEN_RUN_SECONDS=28800`；结束后把状态文件与日志归档到证据目录，并检查 xrun/错误计数。

### 4.4 基线录音（状态：NOT_RUN）

测量信号用仓库内工具生成（纯 Rust，确定性，不依赖 numpy）：

```bash
# 默认：整段 9 s 指数扫频 20 Hz–20 kHz
khaen-sweep --out sweep.wav --rate 48000 --channels 2 --lead 1.5 --tail 1.5 \
    --segment 9:20:20000

# 低频受限时的备选：全频 9 s + 低频专用 9 s（25–63 Hz 积分时间 ×8，用于压低低频噪声影响）
khaen-sweep --out sweep-full-lf.wav --lead 1.5 --gap 2 --tail 1.5 \
    --segment 9:20:20000 --segment 9:20:200
```

`khaen-sweep` 输出一行 JSON（含 sha256、frames、segments），同参数逐字节可复现；
播放前用 sha256 核对信号，基线/施加两段必须用同一个文件。

播放测量信号、用测量麦克风录基线（**不加校准滤波**）：

```bash
# 播放与录音的具体设备/增益按现场接线确定；这里只固定采样率与文件命名
arecord -D "$MIC_PCM" -f S24_3LE -r 48000 -c 1 -t wav baseline.wav &
REC_PID=$!
aplay -D "$KHAEN_ALSA_DEVICE" sweep.wav
wait "$REC_PID"
sha256sum baseline.wav > baseline.wav.sha256
```

要求：无削波（峰值 < −6 dBFS）、时长足够（至少 30 个 4096 点窗）、底噪稳定。

### 4.4b 生成校准方案（离线，可提前演练）

方案文件有两个合法来源，都可在**不碰设备**的情况下提前产出并核对 sha256：

1. **mimo 单点 FIR（推荐，目标①的产物）**：`khaen-single-point --scene ... --out out/` 产出
   `out/single-point/scene-report.json` 与 `out/single-point/fir/*.csv`，再执行：

   ```bash
   khaen-scene-plan --report out/single-point/scene-report.json \
       --fir-dir out/single-point/fir --out calibrated.bin --sample-rate 48000
   ```

   它按报告里的 `outcome_variant`（可用 `--variant` 覆盖）只取该变体的 FIR，按报告的 source
   顺序映射成 lane 0..N-1——不要手动把所有 `fir/*.csv` 按其排序塞进 `khaen-fir-plan`，那会把
   同一源的 B2/B3 两套变体当成两条 lane。手动版 `khaen-fir-plan --fir ...` 仅作为逐文件
   指定 lane 的兜底。
2. **既有 PEQ 设计编译产物**：由 `khaen_calibration::plan_compile` 编译出的方案（保护段
   PEQ + lane 增益），本次改造前的老路径，仍然有效。

产出后先离线跑 `khaen-plan-levels --plan calibrated.bin --out predicted.json`，确认预测电平
合理、与频带中心数量一致，再进入 4.5。预测器现在会同时计入保护段 PEQ/增益与 crossover FIR
幅度（只按幅度，不含群延迟）。

尺寸边界（离线已验证，默认 mimo 抽头 2048）：2 lane × 2048 taps → 准入预算
卷积抽头 4100、估算延迟 1023 样本（48 kHz），host 准入与图编译接受，图内 FIR 按因果延迟输出；
不要超过 ABI 的单 lane 抽头上限（1 048 576）与准入预算（见 `crates/host/src/admission.rs`）。

整链离线演练：`crates/cal-tools/tests/offline_chain.rs` 用合成场景把六个工具按现场顺序串起来
（场景 → FIR CSV → 方案 → 预测电平 → 两段 WAV → 实测电平 → 四文件证据包），断言中间产物
可复现、两次运行逐字节相同。它是 SIMULATED 验收，不代替真机；真机步骤仍是 NOT_RUN。

### 4.5 下发校准方案（状态：NOT_RUN）

两条合法路径，二选一：

1. **rt-host 启动时读方案文件**（推荐用于测量，简单可回滚）：把校准方案文件放到 `/var/lib/khaen/plans/calibrated.bin`，`export KHAEN_PLAN=/var/lib/khaen/plans/calibrated.bin` 后重启 `khaen-rt-host`；启动日志必须出现 `plan applied at version N`。
2. **daemon 两阶段**（用于热切换/回滚演练）：`POST /api/prepare` → `POST /api/commit`；确认 `/api/coeff` 的版本与 graph_id 后，再做测量（`/api/status` 是聚合视图，版本/graph_id 以 `/api/coeff` 为准；两者都需要登录会话）。

   FIR 方案（2048 taps）经 daemon 的 inspect → prepare → commit → 换回恒等 → rollback → commit
   已在离线测试里验证：图内脉冲按预期延迟 1024 帧出现，rollback 后行为逐帧恢复
   （`crates/daemon/tests/rest.rs::fir_plan_roundtrips_through_control_plane`）。真机仍为 NOT_RUN。

`khaen-rt-host` 启动时会先做方案预检（ABI v3 解码 + 准入预算），再交给运行时；失败时
退出码与稳定码可区分问题：读失败 / 解码失败 = 退出 2（`startup_plan_read` / `startup_plan_decode`），
准入拒绝 = 退出 1（`startup_plan_admission_rejected`，消息里带具体预算项）。成功日志会打印
`plan applied at version N (graph_id ...)`，与 daemon 的 `/api/coeff` 对照即可确认同一方案。

禁止：在未确认版本与 graph_id 前开始录音；禁止批量 CueMix 写入。

### 4.6 施加录音（状态：NOT_RUN）

同一测量信号、同一麦克风位置、同一增益，录 `applied.wav`，并记 sha256。两段录音必须同采样率、同设备、同增益。

### 4.7 判定（离线命令，可先在别处演练）

```bash
/opt/khaen/current/bin/khaen-closed-loop-run \
  --plan /var/lib/khaen/plans/calibrated.bin \
  --baseline baseline.wav \
  --applied applied.wav \
  --out /var/lib/khaen/closed-loop/run-YYYYMMDD-HHMM \
  --point P01 --lane 0 --tolerance 2.0 --min-bands 3
```

或包装脚本：`/opt/khaen/current/deploy/closed-loop-verdict.sh` 同上参数。退出码 0/3/4 对应 PASS/FAIL/INCOMPLETE。

### 4.8 不通过时回滚（状态：NOT_RUN）

- rt-host 路径：`export KHAEN_PLAN=/var/lib/khaen/plans/previous.bin` 并重启进程，确认日志版本回退；
- daemon 路径：`POST /api/rollback`，确认 `rolled_back_to` 与 graph_id 回到旧方案；
- 回滚后重跑 4.2 的 30 分钟基线，确认设备仍稳定。

### 4.9 证据归档（状态：NOT_RUN）

把以下内容放进 `HANDOFF/<日期>-<topic>/evidence/` 并附 `SHA256SUMS`：四份复测产物、两段 WAV 的 sha256（WAV 本体按体积规则放 `/var/lib/khaen/` 或 tmp，只记路径与 sha256）、rt-host 状态文件、30 分钟与 8 小时日志摘要。

结论必须区分**实测 / 预测 / NOT_RUN**：`report.json` 里的 verdict 是判定；未在真机执行的步骤一律写 NOT_RUN，不把预测写成实测。

## 5. 判据细节

- 频带：默认 30 个 1/3 倍频程中心（25 Hz–20 kHz）；可用 `--centers` 覆盖，但两段录音与方案必须用同一列表；
- 分析：Hann 窗 Welch 平均（默认窗长 4096、步长一半），带内功率求和；
- 相对电平：`10·log10(实测带功率 / 参考带功率)`；任一侧为 0 记 null（missing）；
- 对齐：方案采样率必须等于录音采样率；两段录音采样率必须一致；
- f32 边界：方案系数是单精度，低频远带预测误差约 1e-2 dB，远小于 2 dB 容差；
- 可复现：同输入重跑四份文件逐字节相同（清单无时间戳）。

## 6. 故障排查（稳定错误码）

| 错误码 | 含义 | 处理 |
| --- | --- | --- |
| `E_USAGE` | 命令行参数缺失/未知 | 对照 `--help` 或本文第 3 节 |
| `E_IO` / `E_WAV_IO` | 文件读失败 | 检查路径与权限 |
| `E_INPUT` / `E_WAV_INPUT` | 文件内容不是合法 WAV / 录音太短 | 重新采集，确认 16/24/32 位 PCM 或 32 位浮点 |
| `E_PLAN` | 方案文件无法解码 | 用 `khaen-fir-plan` 由 FIR CSV 重新产出，或用既有编译器重新编译 |
| `E_PLAN_LEVELS_PLAN` | 方案含非恒等的内容/矩阵/路由段 | 预测只覆盖保护段与 crossover FIR；换用校准方案 |
| `E_BAND_LEVELS_ALIGNMENT` | 两段录音采样率不一致 | 统一录音采样率 |
| `E_CLOSED_LOOP_ALIGNMENT` | 测点集合/频带/采样率对不上 | 检查 `--point`、`--lane`、`--centers` |
| `E_CLOSED_LOOP_RUN_CONFIG` | 测点名/容差/带数/窗长非法 | 测点名只允许 ASCII 字母数字与 `-_.` |
| `E_WRITE` | 输出目录非空或写失败 | 换空目录 |

## 7. 诚实边界

- 预测是方案保护段 + crossover FIR 的**相对频响**，不含房间、单元、麦克风响应；实测侧必须用同参考归一化；
- 不做绝对声压级校准，不做多点校准，不涉及 ADAT；
- 真机行为（ALSA 播放、AES67 接收、MOTU 稳定性）在本仓库里仍是 NOT_RUN，只有设备窗口内的记录才算实测；
- 任何一次 FAIL/INCOMPLETE 都必须原样进交接报告，不重试到通过为止。

## 8. 命令速查

```bash
# mimo 报告 + FIR 目录 -> 方案文件（按报告选定变体，source 顺序即 lane 顺序）
khaen-scene-plan --report out/single-point/scene-report.json --fir-dir out/single-point/fir \
    --out plan.bin --sample-rate 48000
# 兜底：手动指定 FIR CSV（第 i 个 --fir 是 lane i）
khaen-fir-plan --fir fir/SUB1.csv --out plan.bin --sample-rate 48000
# 方案 -> 预测电平
khaen-plan-levels --plan plan.bin --out predicted.json
# 两段录音 -> 实测电平
khaen-band-levels --measured applied.wav --reference baseline.wav --out observed.json
# 预测 vs 实测 -> 三态报告
khaen-closed-loop --predicted predicted.json --observed observed.json --out out/
# 一条命令出完整证据包
khaen-closed-loop-run --plan plan.bin --baseline baseline.wav --applied applied.wav --out evidence/
```
