# tools/board

板端辅助脚本目录。`board.py` 是已有的 paramiko SSH 助手。`run_acceptance.py` 是 T1/T2/T4/T5 验收编排器，只负责下发 runbook 命令、收回产物和写 `run.json`。

12 小时 soak（`khaen-soak12.service`）结束前不要对 `192.0.2.215` 做真实执行。编排器的 live 模式也会拒绝，但不要把“工具会拒绝”当成可以先连上去试的理由。

## 安全

- live 模式连上之后，第一条远程命令是 `systemctl is-active khaen-soak12.service`。stdout 必须恰好是 `inactive`、`failed` 或 `unknown` 之一。其他任何结果（含 `active`、空输出、多行）直接退出 3，不再执行 mkdir、测量、`sed` 或 `systemd-run`。没有绕过开关。
- SSH 连接、认证或超时失败退出 4，不执行测量。
- `--dry-run` 不调用传输层，不导入 paramiko，不打开 socket，因此**读不到**现场 soak 状态。它只打印“门会先跑、但这次没评估”，并列出若门通过后会执行的命令。
- 退出码 0 只表示命令跑完且必需产物已拉回。T1 记录永远是 `partial`，不能写成 T1 PASS。xrun 次数写在 `run.json` 里，需要人读，编排器不会因为 xrun>0 单独改退出码。

| 退出码 | 含义 |
| --- | --- |
| 0 | 过程完成（不是验收通过） |
| 1 | 某一层命令失败，或必需产物缺失；`all` 不再继续后面的层 |
| 2 | 用法错误（缺 `--out`、live 的 T4/all 缺 `--pairs`、路径不合法等） |
| 3 | soak 门未通过 |
| 4 | 板子不可达 |

## 用法

在仓库根目录：

```text
python tools/board/run_acceptance.py --help
python tools/board/run_acceptance.py all --dry-run --out DIR
python tools/board/run_acceptance.py t1 --out DIR --cuemix-state cuemix.json
python tools/board/run_acceptance.py t4 --out DIR --pairs FL:SUB1:80,FR:SUB1:80
# ^ 4.1.2 当前唯一可执行 pair（C-15/B2）；D8：SUB1=Line Out 8/槽7 固定（out10 仅历史临时测试）。5.3.2 标 NEEDS_HW；D3：中置全频设计、当前无中置，中置 HP80/重定向/pair 均 deferred，不参与 pair。
python tools/board/run_acceptance.py t5 --out DIR
```

全局选项可以写在子命令前或后。

| 选项 | 默认 | 作用 |
| --- | --- | --- |
| `--dry-run` | 关 | 只打印命令并写本地 `run.json` |
| `--out DIR` | 必填 | `run.json`、`logs/`、拉回的产物 |
| `--host` | `192.0.2.215` | 板子地址 |
| `--key` | `deploy/ssh/khaen_audio_ed25519` | ed25519 私钥；dry-run 只检查文件存在 |
| `--calibrate-bin` | `/tmp/khaen-calibrate` | 板上二进制，必须是绝对路径且文件名是 `khaen-calibrate` |
| `--pairs` | 无 | live 的 `t4` 和 `all` 必填，原样传给 `merge`；**pair=同一源信号+互补 LR4 分频，fc 必须等于 profile 某 HP/LP 频率**；effect channel（C/SL/SR/HFL/HFR）不参与 pair；D3：5.3.2 中置全频设计、当前无中置，中置 HP80/重定向/pair deferred/NEEDS_HW；当前 4.1.2 只允许 `FL:SUB1:80,FR:SUB1:80`（C-15/B2） |
| `--cuemix-state FILE` | 无 | T1 记录用的 JSON；不给则两个 CueMix 字段为 null |

`--cuemix-state` 必须是对象，且同时包含 `cuemix_routing`（对象或 null）和 `final_insert_direct_routes`（数组或 null）。编排器不内置 in7→out7 等当前路由。

dry-run 的 `all`/`t4` 如果没给 `--pairs`，仍会打印命令，merge 行使用 runbook 示例 pairs，并标明 live 运行会退出 2、不会连接。live 缺 `--pairs` 在连接之前就退出 2。

连接方式与 `board.py` 相同：root、paramiko、`AutoAddPolicy`、超时 20 秒。paramiko 只在 live 传输对象里导入。远程命令用 argv 列表，再经 `shlex.quote` 拼成远端 shell 字符串，本地不用 `shell=True`。

## 各层做什么

命令与 `calibrate/BOARD-T1-T5-RUNBOOK.md` 对齐。SSH 用户是 root，因此去掉了 runbook 里的 `sudo`，避免 tty 密码提示。

- **T1**：先 `arecord -D hw:4,0 --dump-hw-params`（当前 MOTU capture=20ch/48k/S24_3LE），再 `measure-multi --loopback-map 9:7 --names OUT9_IN7 --playback hw:4,0 --playback-channels 22 --capture hw:4,0 --capture-channels 20 --level-db -12 ... --verify-mapping`。**驱动固定 −12 dBFS（不得再用 −30）；前置记录 kMainTrim=0（或实际值+补偿）与 noise_floor_dbfs**；只声称 out9→in7 单路径抽检。
- **T2**：UMIK `hw:3,0`、`--capture-channels 1`、`--channels 0,1 --names FL,FR --mic umik`，`run1`/`run2`/`run3` 各一个目录。
- **T4**：默认/可执行路径是 **4.1.2**：`--channels 0,1,2,3,4,5,7 --names FL,FR,SL,SR,HFL,HFR,SUB1 --mic umik --polarity-offset normal`，然后 `merge --layout 4.1.2 --crossovers 80 --pairs FL:SUB1:80,FR:SUB1:80 --chunksize 128 --negative-controls FILE`；省略 negative-controls = T4 INCOMPLETE。**5.3.2 标 NEEDS_HW**（十只/三 SUB 接好后才验收）：`FL0 FR1 C2 SL3 SR4 SUB1=5 SUB2=6 SUB3=7 HFL=8 HFR=9`，**D3**：5.3.2 中置按设计=全频（与主箱相同），当前无中置 → 中置 HP80/低频重定向/中置 pair/测试全部 deferred/NEEDS_HW、移出 P1；高度/环绕/中置不参与 pair。**D8 已裁决**：SUB1 唯一物理口=**Line Out 8（槽位7）**，命令保持 `--channels 0,1,2,3,4,5,7`；Line Out 10（槽9）仅 2026-11 临时测试历史、不是候选。**D7**：允许 +3 dB boost，但须按 ACCEPTANCE §5 做削波/headroom 检查（单段≤+3.0 dB、每通道正向和 >6 dB warning/>12 dB 拒绝、预测最大累计峰值≤−6 dBFS、实测峰值≤−1 dBFS），runbook merge 加 `--boost-cap-db 3` 并在报告记录 `boost_filters/total_boost_db/max_predicted_peak_dbfs/headroom_db/measured_peak_dbfs`。不跑逐通道 `fit`（runbook 写 merge 会内部重拟合）。
- **T5**：先 64 再 128，两套记录互不覆盖。64 使用 runbook 的 `sed` 与 `systemd-run --unit=khaen-t5-64 ... soak12.sh 1`。128 使用反向 `sed` 和 `--unit=khaen-t5-128`。读回 `camilla.yml` 的 chunksize，不一致就不起 `systemd-run`。每轮结束后把 `status.csv` 和 `camilla.log` 拷到 `/var/lib/khaen-calib/t5/chunksize-64` 与 `chunksize-128`，再 SFTP 拉回。

`uncovered_outputs_1based` 取 5.3.2 的物理输出 1..10 里除 9 以外的 9 个：`[1,2,3,4,5,6,7,8,10]`。MOTU 播放设备仍是 22 声道；11..22 不在这个列表里，也不声称已验证。runbook 第 29 行的 `1..22` 列表没有照抄。

T1 的 `claim` 使用 runbook 第 62 行：`T1: partial (out9->in7 only); 1:1 mapping and other channels not verified`。

## run.json

写在 `<out>/run.json`。每步先写 `run.json.tmp`，再 `os.replace`。原始 stdout/stderr 在 `<out>/logs/`。

顶层字段：

| 字段 | 内容 |
| --- | --- |
| `schema` | `khaen-board-acceptance-run/v1` |
| `tool` | `name` + `version` |
| `invoked_arguments` | 本次 argv |
| `dry_run` | 是否演练 |
| `started_at` / `updated_at` | ISO-8601，带时区偏移 |
| `exit_code` | 上表 |
| `board` | `host`、`user`、`port`、`identity`（hostname / uname / date / asound_cards） |
| `precondition` | soak 检查列表；dry-run 时 `evaluated` 为 false，`detail` 为 `not-evaluated` |
| `commands` | 每条外部命令的 `argv`、`tier`、`started_at`、`finished_at`、`exit_code`、`stdout`、`stderr` |
| `artifacts` | 远端路径、本地相对路径、`required`、`present`、`sha256`、`bytes` |
| `tiers` | 各层 `subruns[].meta` |
| `assumptions` / `warnings` / `error` | 口径与失败原因 |

T1 的 `meta.t1` 含 ACCEPTANCE §3.1 的 `status=partial`、`loopback_map_1based`、覆盖/未覆盖列表、`cuemix_routing`、`final_insert_direct_routes`、`mitigation`、`claim`。CueMix 两字段只来自 `--cuemix-state`，否则为 null 并带 warning。

T4 merge 的 `meta.chunksize` 优先取拉回的 `calibration.yml.report.json` 里 `environment.chunksize`，否则取实际发出的 `--chunksize` argv。两边不一致时记 `chunksize_mismatch`，不另写一个死常量。

T5 的两个 subrun（`chunksize-64`、`chunksize-128`）各自有 `chunksize`（来自 camilla.yml 读回）、`unit`、`service`、`period_size`、`buffer_size`（从日志解析，没有就保持 null）、`remote_status_csv`、`remote_camilla_log`、`xrun`。

产物：测量层必需 `*.fr.csv`、`*.analysis.json`、`*.diag.json`（T1 为 `OUT9_IN7-ch08.*`）。T4 另外必需 `calibration.yml`、`calibration.yml.pairs.json`、`calibration.yml.report.json`。T5 每轮必需 `status.csv` 和 `camilla.log`。缺必需文件则该层失败。可选文件（如 `cuemix-routing.png`、validation、camilladsp 片段）缺失只记 warning。

## 测试

```text
python -m unittest discover -s tools/board/tests -v
```

只使用标准库。不连接板子。

## 还不做的事

最终 insert（关闭全部 CueMix input→output、截图 `final-insert/cuemix-final.png`）不在本编排器里。**D7 boost 的 `--boost-cap-db 3` 目前只在 runbook merge 命令中；编排器 T4 尚未透传该选项，D7 验收以 runbook 命令为准（工具补齐另立任务）。

**T5 时长单位已定稿（B3）：`soak12.sh` 的参数是小时，`1`=3600 s；runbook 与 ACCEPTANCE §7 均为「64/128 各至少 60 min（3600 s），10 min 只作 screening、不得写 PASS」**；不再使用「600 秒」旧口径。live T5 前仍由人确认板端窗口。
