# NSP7-DAEMON-RUNBOOK —— 常驻 NSP 7ch 链路部署/操作手册

> 2026-11-22 由 `aes67-daemon` 任务首次部署并实测。**当前状态：已启用（active+enabled）**。
> 实测数字/证据：`evidence/aes67-daemon-20261122/REPORT.md`；生产化方案背景：`deploy/NSP7-PRODUCTION-PLAN.md`。
> 安全底线：CueMix **koTrim[0..11]=100 + kMainTrim=100**（物理静音）才允许启停；出声窗口恢复音量是另一个动作。

## 1. 组成

| 组件 | 仓库源 | 板上安装位置 |
|---|---|---|
| systemd unit | `deploy/config/tspi-rt-host-nsp7.service` | `/etc/systemd/system/tspi-rt-host-nsp7.service` |
| host 配置 | `deploy/config/rt-host-nsp7.toml.example` | `/etc/tspi-rt-stack/rt-host-nsp7.toml` |
| NSP-first 闸门 | `deploy/config/nsp7-wait-stream.sh` | `/usr/local/lib/tspi-rt-stack/nsp7-wait-stream.sh` |
| host 二进制 | 板端 `/root/candidate/day-window/bin/tspi-rt-host`（sha f510ed64…） | `/usr/local/lib/tspi-rt-stack/f510ed64/tspi-rt-host` |
| NSP SDP | NSP `observed/aes67.sdp` 副本（sha 731ea83c…） | `/var/lib/tspi-rt-stack/nsp-aes67.sdp` |
| plan | 8-in/7-role identity ABI v1（sha 52f2f3ad…） | `/var/lib/tspi-rt-stack/plans/plan-412-identity.bin` |
| 操作脚本 | `deploy/tools/nsp7-*.sh` | `/root/aes67-rx-work/tools/` |

/opt 生产二进制 **不复用也不替换**：`775bdb23…` 构建早于 userspace AES67 路径。unit 的 ExecStart 指向
`/usr/local/lib/…/f510ed64/tspi-rt-host`。

## 2. 安装/升级步骤（静音窗口，操作员执行）

```bash
# 0) 静音确认（fresh 回读）：koTrim[0..11]=100, kMainTrim=100；UMIK 基线
# 1) 二进制与数据
install -d -m 0755 /usr/local/lib/tspi-rt-stack/f510ed64
install -m 0755 /root/candidate/day-window/bin/tspi-rt-host /usr/local/lib/tspi-rt-stack/f510ed64/tspi-rt-host
install -m 0755 nsp7-wait-stream.sh /usr/local/lib/tspi-rt-stack/nsp7-wait-stream.sh
install -d -m 0755 /var/lib/tspi-rt-stack/plans
cp /root/aes67-rx-work/chain/plan-412-identity.bin /var/lib/tspi-rt-stack/plans/
cp /root/aes67-rx-work/chain/nsp-aes67.sdp     /var/lib/tspi-rt-stack/
# 2) 配置/unit
install -m 0644 rt-host-nsp7.toml.example /etc/tspi-rt-stack/rt-host-nsp7.toml
install -m 0644 tspi-rt-host-nsp7.service  /etc/systemd/system/tspi-rt-host-nsp7.service
cp -a /etc/systemd/system/tspi-rt-stack.target /etc/systemd/system/tspi-rt-stack.target.bak-nsp7-$(date +%Y%m%d)
# 3) 启动关系：target Wants/After 的 tspi-rt-host.service -> tspi-rt-host-nsp7.service（手工编辑或用 evidence installed/ 里的样例）
# 4) 停旧 standalone rx（释放 5004 + ALSA），再 enable/start
bash /root/aes67-rx-work/tools/stop-rx.sh
systemctl daemon-reload
systemctl enable tspi-rt-host-nsp7
systemctl disable tspi-rt-host.service      # 旧 unit 保留文件、可回滚
systemctl start tspi-rt-host-nsp7            # wrapper 等 NSP RTP 后再 exec host
```

## 3. 启动顺序（NSP-first，硬要求）

```
CueMix 静音 → NSP 已在发流（sender-host → 192.0.2.222:5004，7ch L24/48k）→ systemctl start tspi-rt-host-nsp7
```
host 的 `aes67_stall_blocks=400`（≈1.07 s，2× 后 fail-closed）等不到 NSP 会话 6–10 s 启动；
unit 的 ExecStart 是 `nsp7-wait-stream.sh`，用 `tcpdump 'udp dst port 5004'` 等到 RTP 才 exec host。
NSP 中断 >2.1 s → host fail-closed → systemd 2 s 重启 → wrapper 重新等流（有短暂无声间隙，属预期）。

## 4. 验收

```bash
bash /root/aes67-rx-work/tools/nsp7-status.sh
systemctl is-active tspi-rt-host-nsp7
tail -n 3 /var/log/tspi-rt-stack/host-nsp7-run.err      # 期望空
```
判据：unit active、唯一 1 个 tspi-rt-host、5004 仅 host 持有、card1 pcm0p RUNNING owner=host；
host 退出报告 `status=ok`、`flow_layout=4.1.2-7ch-sMPTE2110(41,2)`、`jitter_latency_us=64000`、
`loss_events=0`、`stall_events=0`、`alsa.xruns=0`；CPU 34–37 %（4C）、RSS ≈17.6 MB、xruns=0；
UMIK 基线（−73…−74 dBFS RMS）。之后每次 host 退出会在
`/var/log/tspi-rt-stack/host-nsp7-run.jsonl` 追加一份 JSON 报告。

## 5. 回滚

```bash
# 一键（本会话）：停 nsp7 → 启 standalone rx supervisor
bash /root/aes67-rx-work/tools/rollback-to-rx.sh
# 永久：恢复旧 unit/启动关系
systemctl disable --now tspi-rt-host-nsp7
cp /etc/systemd/system/tspi-rt-stack.target.bak-nsp7-20261122 /etc/systemd/system/tspi-rt-stack.target
systemctl daemon-reload
systemctl enable tspi-rt-host.service
bash /root/aes67-rx-work/tools/start-rx.sh
```
回滚后复核 CueMix 仍静音、5004/ALSA 只有预期 owner。CueMix 恢复音量只在授权出声窗口：
`bash /root/aes67-rx-work/tools/restore-cuemix.sh`。

## 6. 限制 / NOT_RUN（不得当作已解决）

1. **60 ppm 时钟漂移**：无 PTP/PHC，64 ms 抖动缓冲只延后耗尽（4 ms ≈64–103 s；10 min PASS @64 ms）；
   长期需 PTP/ASRC 或 host epoch reopen（未实现）。systemd 自动重启 = 中断后恢复，不是无间断。
2. **not_due 7.4–8.5 %**（无 PTP 相位差整块静音，非丢包）。
3. **开机自启已 enable，reboot 未验证**；预期顺序 network-online → irq-affinity → daemon/nsp7 → wrapper 等流 → host。
4. **apply_affinity=false**：RT 亲和/mlockall 未启用（4C 板需单独窗口验证）；deadline_misses 0.1–0.3 %，无 xrun。
5. **plan_abi=2/IIR 未进 7ch 链路**：无 8-in ABI v2 plan；flat/tilt10（2-in）被 7ch userspace 拒收。先产出 8-in
   ABI v2 plan，再按本手册灰度；flat ABI v2 的 parse/manifest/IIR 段数已在 fake/synth dry-run 验证。
6. MOTU 表内容验证 NOT_RUN（当前 NSP 会话无音频内容）；声学测试按要求未做。

## 7. 出声窗口 / 让位其他 owner（2026-11-22 实测）

```bash
# 用户切到出声试听（如游戏 NSP）：CueMix 从夜间 mute 恢复到低电平（kMainTrim=21）
bash /root/aes67-rx-work/tools/nsp7-restore-cuemix-low.sh
# 给其他 owner（MIMO A/B 等）让出 card1：优雅停止，不 disable
systemctl stop tspi-rt-host-nsp7          # card1 closed + 5004 释放；CueMix 不动
# 对方确认释放后：重启同一 host 并 30 s 观测
bash /root/aes67-rx-work/tools/nsp7-restart-after-mimo.sh
```

- **xrun 精确计数只在 host 优雅退出时写入 report**；live 只能验证 `state=RUNNING`、hw_ptr 单调、无 XRUN/SETUP。
  需要某段运行的 xrun 数时，在该段结束优雅 stop 后读 `/var/log/tspi-rt-stack/host-nsp7-run.jsonl` 最后一行。
- 2026-11-22 实测：出声窗口 50 s（游戏试听）report = status ok / xruns 0 / underruns 0 / loss 0 / stall 0 /
  not_due 0.17 %；为 MIMO A/B 让出两次（11:32:58、11:34:39），11:46:02 重启并验证 streaming（PID 237769，
  CPU 34.7 %，RSS 18,000 kB，NSP ≈908 pps）。
- 恢复夜间静音（用户结束试听、Lead 要求时）：`koTrim[0..11]=100 + koBusMute[0..13]=1 + kMainTrim=100`。
