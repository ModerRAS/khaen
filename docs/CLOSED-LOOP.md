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

## 3. 五个离线命令行工具

| 工具 | 作用 | 退出码 |
| --- | --- | --- |
| `khaen-single-point` | 合成场景 → 求解 → 参数/报告/方案/清单 | 0 成功 / 3 PARTIAL / 4 NONE / 2 输入 / 1 运行 |
| `khaen-plan-levels` | 方案文件 → 预测电平（predicted 侧） | 0 / 2 输入 / 1 写失败 |
| `khaen-band-levels` | 两段 WAV → 实测相对电平（observed 侧） | 0 / 2 输入 / 1 写失败 |
| `khaen-closed-loop` | 两份电平集合 → 三态报告 | 0 PASS / 3 FAIL / 4 INCOMPLETE / 2 输入 / 1 写失败 |
| `khaen-closed-loop-run` | 方案 + 两段 WAV → 四文件证据包 | 同上三态 + 2 输入 + 1 写失败 |

`deploy/closed-loop-verdict.sh` 是设备侧薄包装：检查文件与输出目录后调用 `khaen-closed-loop-run`，退出码透传。语法经 `bash -n` 检查，真机行为 NOT_RUN。

## 4. 现场操作序列（窗口内执行；每步标注状态）

前置条件（未满足就停）：

- 用户明确许可出声窗口；当前时段在 08:30–12:00 或 14:30–18:00 内；
- 已知 MOTU 的 ALSA PCM 设备名（只读 `aplay -l` 获取）；
- 已安装发布包：`/opt/khaen/current/bin/` 下有 `khaen-daemon`、`khaen-rt-host` 与五个校准命令行工具；
- 状态目录 `/var/lib/khaen/` 可写。

### 4.1 只读枚举与设备健康（状态：NOT_RUN）

```bash
aplay -l                        # 只读枚举：记录 MOTU 的 card 号、card id 与 PCM 名
aplay -L | head -40             # 只读：确认有哪些 PCM（别名不一定含 motu）
cat /proc/asound/cards          # 只读：card id 必须以此为准，不要凭记忆猜
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

判据：日志无 `run_block failed`、状态文件里 `output_errors` 为 0、无 xrun 告警。

### 4.3 8 小时浸泡（状态：NOT_RUN）

同 4.2 参数，`KHAEN_RUN_SECONDS=28800`；结束后把状态文件与日志归档到证据目录，并检查 xrun/错误计数。

### 4.4 基线录音（状态：NOT_RUN）

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

### 4.5 下发校准方案（状态：NOT_RUN）

两条合法路径，二选一：

1. **rt-host 启动时读方案文件**（推荐用于测量，简单可回滚）：把校准方案文件放到 `/var/lib/khaen/plans/calibrated.bin`，`export KHAEN_PLAN=/var/lib/khaen/plans/calibrated.bin` 后重启 `khaen-rt-host`；启动日志必须出现 `plan applied at version N`。
2. **daemon 两阶段**（用于热切换/回滚演练）：`POST /api/prepare` → `POST /api/commit`；确认 `/api/status` 的版本与 graph_id 后，再做测量。

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
| `E_PLAN` | 方案文件无法解码 | 用 `khaen-single-point` 重新产出方案 |
| `E_PLAN_LEVELS_PLAN` | 方案含非恒等的内容/分频/矩阵/路由段 | 预测只覆盖保护段；换用校准方案 |
| `E_BAND_LEVELS_ALIGNMENT` | 两段录音采样率不一致 | 统一录音采样率 |
| `E_CLOSED_LOOP_ALIGNMENT` | 测点集合/频带/采样率对不上 | 检查 `--point`、`--lane`、`--centers` |
| `E_CLOSED_LOOP_RUN_CONFIG` | 测点名/容差/带数/窗长非法 | 测点名只允许 ASCII 字母数字与 `-_.` |
| `E_WRITE` | 输出目录非空或写失败 | 换空目录 |

## 7. 诚实边界

- 预测是方案保护段的**相对频响**，不含房间、单元、麦克风响应；实测侧必须用同参考归一化；
- 不做绝对声压级校准，不做多点校准，不涉及 ADAT；
- 真机行为（ALSA 播放、AES67 接收、MOTU 稳定性）在本仓库里仍是 NOT_RUN，只有设备窗口内的记录才算实测；
- 任何一次 FAIL/INCOMPLETE 都必须原样进交接报告，不重试到通过为止。

## 8. 命令速查

```bash
# 方案 -> 预测电平
khaen-plan-levels --plan plan.bin --out predicted.json
# 两段录音 -> 实测电平
khaen-band-levels --measured applied.wav --reference baseline.wav --out observed.json
# 预测 vs 实测 -> 三态报告
khaen-closed-loop --predicted predicted.json --observed observed.json --out out/
# 一条命令出完整证据包
khaen-closed-loop-run --plan plan.bin --baseline baseline.wav --applied applied.wav --out evidence/
```
