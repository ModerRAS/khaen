# NSP 7ch -> tspi-rt-host 生产化方案（草案，未启用）

> 来源：motu-meter-dsp 任务 2026-11-22 静音窗口实测（REPORT §4.5/§4.6、RUNBOOK §6/§7）。
> 状态：**2026-11-22 已由 aes67-daemon 任务安装并启用**（`tspi-rt-host-nsp7.service` active+enabled，全程静音）。
> 实施结果/偏差见 §9，证据 `evidence/aes67-daemon-20261122/`；操作入口 `deploy/NSP7-DAEMON-RUNBOOK.md`。
> 下文的“草案/未启用”表述为原始设计记录，当前以 §9 为准。

## 1. 目标与边界

- 用 **tspi-rt-host 单进程**替换 standalone aes67-rx：AES67 RX（userspace 7ch 4.1.2）→ plan-arena DSP →
  ALSA MOTU，唯一 ALSA owner，CueMix 静音兜底。
- 输入流就是 **NSP 现有 7ch 4.1.2 SMPTE2110.(41,2)** 流（目的 192.0.2.222:5004），不再需要改 NSP 发送格式。
- 本方案不改 NSP 的 objects/PCM 捕获逻辑；NSP 侧 PCM 构建的激活是前置条件（见 §5）。

## 2. 部署文件（本目录）

| 文件 | 安装位置 | 说明 |
|---|---|---|
| `config/rt-host-nsp7.toml.example` | /etc/tspi-rt-stack/rt-host-nsp7.toml | host 配置（plan-arena + userspace 7ch + ALSA） |
| `config/rt-host-nsp7.env.example` | /etc/tspi-rt-stack/rt-host-nsp7.env | 环境覆盖（可选） |
| `config/tspi-rt-host-nsp7.service` | /etc/systemd/system/tspi-rt-host-nsp7.service | 独立 unit；与旧 tspi-rt-host.service 互斥使用 |
| `config/nsp7-wait-stream.sh` | /usr/local/lib/tspi-rt-stack/nsp7-wait-stream.sh | **NSP-first 启动闸门**（等 udp/5004 RTP 后再 exec host）；unit 的 ExecStart |
| `tools/nsp7-switch-to-host.sh` / `nsp7-rollback-to-rx.sh` / `nsp7-status.sh` | /root/aes67-rx-work/tools/ | 一键切换/回滚/状态快照（板上操作用） |

数据文件（不进 repo）：
- `/var/lib/tspi-rt-stack/nsp-aes67.sdp`：NSP 会话 `observed/aes67.sdp` 的拷贝（7ch、L24/48k、SMPTE2110.(41,2)）；
  实测安装值 sha256 `731ea83cffce56bd9cdf5046089ba49535dc2c2e15377ad98f1b9f6fcd9aa520`（361 B）。
- `/var/lib/tspi-rt-stack/plans/plan-412-identity.bin`：已验收 4.1.2 plan（测试/生产按 sha 记录）；
  **实际生产使用** = 8-in/7-role identity ABI v1，1636 B，sha256 `52f2f3ad…`（graph_id `64acfb34…`）。
- `/var/lib/tspi-rt-stack/plans/plan-412-flat.bin`：RUN2 flat 真 plan（4860 B，ABI v2，sha `3fced8bc…`）已上传，
  但它是 **2-in** 设计，tspi-rt-host 的 7ch userspace 只接受 8ch strict 图输入 → 直接接入被拒（详见 §9）。

## 3. 切换步骤（静音窗口；手动 run 先行，通过后再上 unit）

```
# 0) 安全前提：CueMix koTrim[0..11]=100 + kMainTrim=100；Umik 证明无声；记录 before 快照/restore 计划
# 1) 停 standalone rx（释放 5004 + ALSA）
bash /root/aes67-rx-work/tools/stop-rx.sh
# 2) 准备 SDP/plan（sha256 记录到证据）
sha256sum /var/lib/tspi-rt-stack/nsp-aes67.sdp /var/lib/tspi-rt-stack/plans/plan-412-identity.bin
# 3) 手动 run 验收（与 unit 相同 ExecStart 等价）
/opt/tspi-rt-stack/current/bin/tspi-rt-host run --config /etc/tspi-rt-stack/rt-host-nsp7.toml \
  > /var/log/tspi-rt-stack/manual-nsp7.json 2> /var/log/tspi-rt-stack/manual-nsp7.err
# 4) 验收（另开终端；见 §4）
# 5) 停手动 run（SIGTERM 优雅退出）→ install unit → systemctl start tspi-rt-host-nsp7
# 6) 跟随 unit 日志验收 again；systemctl enable tspi-rt-host-nsp7（开机自启）
```

## 4. 验收判据（任一不满足即回退）

- host JSON：`status=ok`、`graph=plan-arena`、`capture.flow_layout=4.1.2-7ch`、plan sha 正确、
  `capture.loss_events=0`、`stall_events=0`、`alsa.xruns=0`、`underruns=0`；
- MOTU 表（先临时解 koBusMute[0..7]，测完恢复）：Main1/Main2 随 NSP 内容动，Line7（4.1.2 空槽）恒 −inf；
- Umik：RMS/峰值不高于环境基线（本房间约 −75.7…−76.5 dBFS RMS）；
- CPU/内存：4C 板 CPU≈36–50%（取决于 plan），RSS 稳定（soak 10 min 无增长）；
- 2026-11-22 实测基线：6 s/2250 blocks，xruns=0/underruns=0/loss=0，not_due 0–1（默认 4 ms），
  CPU 36.4%，RSS≈17.7 MB；10 min 静音 soak 结果见 RUNBOOK §7.5。

## 5. NSP 开机自动方案（三选一；均未实测，列风险）

### A. 登录触发计划任务（推荐 v1）
- 原理：`schtasks /Create /TN NSP-StopNsp /SC ONLOGON /RL HIGHEST /RU adqew /TR "powershell -File <wrapper>"`；
  wrapper 起 `FinalSpatialAudio.exe --start-nsp`。
- 优点：FSA 需要交互式 session 2 才能创建托盘/音频端点（实测 session 0 启动会 "Tray creation failed"）；
  PCM 构建在登录会话下可正常安装 hook（实测）。
- 风险：必须有人登录（无头/无人值守不适用）；UAC 凭据、RDP 断开会话、explorer/tray 崩溃影响；
  登录风暴时需等待 NSP 会话稳定（见 §6 竞态）。
- 需要：adqew 是管理员；任务用 /RL HIGHEST（hook 注入 audiodg 需要 elevated 权限）。

### B. 服务/headless（需 NSP 改造，NOT_RUN）
- 原理：把 FSA 做成 Windows 服务或去掉托盘依赖，在 session 0 直接跑。
- 现状：实测 session 0 启动 FSA 直接 "Tray creation failed hr=0x80004005" 退出 → 不能直接用服务化。
- 需要：NSP 侧提供 headless 模式（NOT_RUN；不在本任务写域）。

### C. 混合：登录任务 + 健康检查/重试
- A 的 wrapper 增加健康检查：session running + `observed/aes67-live.json` framesSubmitted 增长、
  silentPackets 不增长；失败则 stop→清 stale session→restart Audiosrv→重试（见 §6）。
- 推荐在 A 上叠加，作为生产开机脚本。

## 6. NSP PCM 激活的竞态与坑（实测）

1. FSA **必须在 session 2 交互式 + elevated** 启动（§5 A），否则 PCM hook 控制失败；
2. 启动前必须清 stale `session.json/last-error.json/hook-status.*` 并 `Restart-Service Audiosrv`，
   否则 FSA 恢复路径先装 objects hook，PCM 被 "Object capture hook resident; PCM capture is mutually exclusive" 拒绝；
3. 切到 PCM 前确认 audiodg 无 objects hook 模块；切回 objects 时反向确认无 pcm hook；
4. 生产启动顺序建议：先 CueMix 静音 → start tspi-rt-host（等待 SOF/无输入也不 fail）→ 再起 NSP PCM 会话；
   **注意**：host 的 `aes67_stall_blocks` 失流门限（400 blocks≈1.07s，2× 后 fail-closed）可能早于 NSP 起来
   → 生产必须让 NSP 先出流再启 host，或临时放大 stall 门限（≤1000，2.67s）。本任务未实测 host-first。

## 7. 回退

```
systemctl stop tspi-rt-host-nsp7
systemctl disable tspi-rt-host-nsp7   # 如需永久回退
bash /root/aes67-rx-work/tools/start-rx.sh          # 恢复 standalone rx
# CueMix 保持静音；白天出声窗口再执行 restore-cuemix.sh
```

## 8. 风险清单

| 风险 | 影响 | 缓解 |
|---|---|---|
| 无 PTP（UsrvClock） | 长稳可能漂移；本窗口 not_due 0–1/2250，但 33% 异常曾出现一次且未复现 | 接 PTP/PHC；soak 监控 not_due；异常时 fail-closed |
| NSP PCM 构建非正式发布 | 重新构建 hash 与窗口验证版不同（同尺寸） | 固化构建脚本/产物 hash 到 NSP 仓库（NOT_RUN）；生产用已验证 hash |
| NSP 登录任务/服务化未验证 | 开机自动可能失败或起不来 | 先手动+健康检查；v1 用登录任务；保留手动切换 runbook |
| host-first vs NSP-first 竞态 | 失流 fail-closed 退出 | 生产按 NSP-first 顺序；或放大 stall 门限并验收 |
| CueMix 静音非 host 控制 | 误启动可能出声 | 启动前静音检查清单 + Umik 验证；unit 不加 ExecPre 自动放音 |
| apply_affinity 未在 4C 板验证 | RT 调度失败/fail-closed | 首次切换 apply_affinity=false；单独窗口验证 true |


## 9. 实施结果与偏差（2026-11-22，aes67-daemon 任务）

> 证据：`evidence/aes67-daemon-20261122/REPORT.md`（数字+索引）、`RUNBOOK.md`、`raw/`、`installed/`。

### 9.1 实际状态

| 项 | 值 |
|---|---|
| unit | **`tspi-rt-host-nsp7.service`**：active (running)、**enabled（开机自启，reboot 未验证）**；旧 `tspi-rt-host.service` disabled/inactive |
| host 二进制 | `/usr/local/lib/tspi-rt-stack/f510ed64/tspi-rt-host`（`f510ed64…`，task-284 修复版）；**/opt `775bdb23…` 未替换**（其构建早于 userspace AES67 路径） |
| 配置 | `/etc/tspi-rt-stack/rt-host-nsp7.toml`（jitter 64000 µs、ring 8192、stall 400、apply_affinity=false） |
| SDP / plan | `nsp-aes67.sdp`（`731ea83c…`）/ **8-in/7-role identity ABI v1**（`52f2f3ad…`） |
| 启动顺序 | NSP-first：`nsp7-wait-stream.sh` 等 udp/5004 RTP 出现后才 exec host；systemd `Restart=on-failure/RestartSec=2/StartLimitIntervalSec=0` |
| 启动关系 | `tspi-rt-stack.target` 的 Wants/After 已从旧 host 改为 nsp7（备份 `/etc/systemd/system/tspi-rt-stack.target.bak-nsp7-20261122`） |
| 回滚 | `bash /root/aes67-rx-work/tools/rollback-to-rx.sh`（一键停 host→启 rx supervisor）；永久回滚见 evidence RUNBOOK §3 |

### 9.2 实测验收（两段静音 soak，through systemd）

- unit active；`flow_layout=4.1.2-7ch-sMPTE2110(41,2)`、`jitter_latency_us=64000`、plan `parse=t20-ok admission=platform`；
- ALSA **xruns=0 / underruns=0 / overruns=0 / recoveries=0 / faults=0**；capture `loss_events=0 / stall_events=0 / underrun_blocks=0`；
- CPU **33.9–35.1 %**（4C）、RSS **≈17.6 MB 恒定**、UMIK **−73.2…−73.9 dBFS RMS**（静音基线内）；
- NSP-first 日志实测生效；`kill -9` host 后 systemd 2 s 自动重启并重新拿到 5004+ALSA（进程级自愈通过）。
- 与参考手动 10 min soak（not_due 7.97 %/CPU 37.2 %/xruns 0）一致；本次 not_due 7.42–8.53 %。

### 9.3 与草案的偏差（如实）

1. **plan 不能用 flat/tilt10**：RUN2 flat/tilt10（4860 B，ABI v2）是 **2-in** 设计（`plan.adapter.n_src=2`），
   7ch userspace flow 要求 8ch strict 图输入，实测 `ERR_ROLES: …只支持 8ch strict(4.1.2) 图输入（got 2）`。
   → 常驻链路改用 window-nsp-e2e 已验证的 8-in/7-role identity（ABI v1）。
   flat ABI v2 只在 fake/synth dry-run 验证 `plan_abi=2, role_iir_roles=2, role_iir_sections=10, manifest=verified`。
   **plan_abi=2/IIR 进 7ch 链路 = NOT_RUN**；需先产出 8-in ABI v2 plan。
2. **host 二进制不在 /opt**：candidate f510ed64 安装到 `/usr/local/lib/`，/opt 775bdb23 原样保留（按任务约束未替换）。
3. **NSP-first 由 wrapper 实现**（不是 host 原生等待）：host-first 会在 ~2.1 s 失流 fail-closed；wrapper 用 tcpdump 等流。
4. **开机自启已 enable 但未 reboot 验证**（NOT_RUN）；预期 boot 顺序见 `deploy/NSP7-DAEMON-RUNBOOK.md`。
5. **长稳仍未解决**：无 PTP/PHC，≈60 ppm 漂移；64 ms 只延后耗尽（参考 10 min PASS）。当前自动重启=中断恢复，非无间断。
6. **MOTU 表内容验证 NOT_RUN**：当前 NSP 会话内容为静音；解 bus mute（koTrim 仍 100）后表仍 −inf，
   UMIK 证静音。DSP 有效性依赖既有 window-nsp-e2e 证据 + 本次 plan/ALSA/帧数证据。
7. `apply_affinity=false`（RT 亲和/mlock 未启用），report 如实 `kernel_applied=false`；deadline_misses 0.1–0.3 %，无 xrun/safety-mute。
