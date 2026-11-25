# TSPI 重启持久性 / 恢复检查清单（B1 开机 IRQ 竞态）

> 用途：**板子每次上电/重启后立即**按本清单只读确认新栈状态，重点确认 B1（IRQ 竞态）不再复发。
> 适用：release-v12 起（B1 修复已入库：unit `ExecStartPre=irq-affinity-pin wait`）；release-v11 及更早
> 仍依赖板端 live-only drop-in，本清单“§2 legacy drop-in”检查会显示不同结果。
> 纪律：只读为主；标 **[WRITE]** 的命令会改变运行状态，必须在授权窗口内、以 root 执行。**任何情况下不得恢复旧 C
> （CamillaDSP/tspi-upmix/tspi-audio.target）**。清单不替代 T71 授权窗口与 `deploy/README.md` 主 runbook。
> 证据目录：`rt-stack/evidence/irq-boot-persist/`；B1 现场：`rt-stack/evidence/board-window-20261120/FINDINGS.md`。

## 0. 60 秒总览（一条命令看结论）

```bash
systemctl is-active tspi-irq-affinity.service khaen-rt-host.service tspi-rt-daemon.service tspi-rt-stack.target
cat /proc/irq/58/smp_affinity_list /proc/irq/56/smp_affinity_list /proc/irq/57/smp_affinity_list   # 期望 1 2 3
curl -sS -o /dev/null -w 'healthz=%{http_code}\n' http://127.0.0.1:80/healthz                         # 期望 200（daemon 在 :80；8090 仅 127.0.0.1 probe）
```

期望：`irq-affinity active` / `host active` / `daemon active` / `target active` + **1 2 3** + **healthz=200**。
任一不符 → 按 §1–§8 定位；**先看 irq-affinity，再看 host**（B1 的典型症状是 host inactive 而 target/daemon active）。

## 1. B1 判定入口：先查 irq-affinity origin（host inactive 时必查）

```bash
systemctl status tspi-irq-affinity.service --no-pager -l
journalctl -b -u tspi-irq-affinity.service --no-pager | tail -n 50
```

- **v12+ 期望**：`ExecStartPre` 先出现 `irq-affinity: OK 依赖 IRQ 的 /proc/irq 节点已就绪（waited=…）` 与
  `RESULT PASS（action=wait waited=…）`，随后 `RESULT PASS（action=apply …）`，unit = `active (exited)`。
- **v11 legacy（live-only drop-in）**：日志来自 `timeout 60 /bin/sh -c until …`，语义相同；若同一 unit 同时有
  `ExecStartPre`（unit 内置）与 legacy drop-in 两条等待，说明 drop-in 未清理（重跑 04 会自动迁移/删除）。
- **FAIL（重启后 host 不启动）**：日志出现 `FAIL 依赖 IRQ 的 /proc/irq 节点在 60s 内未就绪: IRQ56 IRQ57` →
  进入 §8 判定表（IRQ 号变化/设备未枚举，**不要**用 `systemctl start khaen-rt-host.service` 绕过钉核前置）。

## 2. IRQ 钉核 + B1 前置是否真的装到板上

```bash
for i in 58 56 57; do printf 'IRQ%s=' "$i"; cat /proc/irq/$i/smp_affinity_list; done    # 期望 IRQ58=1 IRQ56=2 IRQ57=3
systemctl is-enabled irqbalance 2>/dev/null; systemctl is-active irqbalance 2>/dev/null  # 期望 disabled + inactive
grep -c '^ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait$' /etc/systemd/system/tspi-irq-affinity.service   # 期望 1
test -e /etc/systemd/system/tspi-irq-affinity.service.d/10-wait-irq.conf \
  && echo 'WARN: legacy live-only drop-in 仍存在（重跑 04 或删除后 daemon-reload）' || echo 'legacy drop-in: 无（good）'
systemctl cat tspi-irq-affinity.service | grep -E 'ExecStartPre|TimeoutStartSec'          # 期望 wait 前置 + TimeoutStartSec=120
```

判定：IRQ 必须是 **1/2/3**（A53）。出现 7/3/5（A72）说明 irqbalance 复活或 oneshot 未执行 → §8。

## 3. 栈单元与 host 日志

```bash
systemctl is-enabled tspi-rt-stack.target
systemctl is-active tspi-rt-stack.target khaen-rt-host.service tspi-rt-daemon.service
systemctl show -p NRestarts --value khaen-rt-host.service                      # 期望 0（启动即崩会 >0）
journalctl -b -u khaen-rt-host.service --no-pager | grep -E 'kernel_applied|scheduler|SAFE_MODE|healthz' | tail -n 20
journalctl -b -u tspi-rt-daemon.service --no-pager | tail -n 20
```

期望：host 日志含 `kernel_applied=true`（T45 实写内核）+ `SAFE_MODE=mute`（当前终态静音）或 normal；
daemon 日志无 panic/重启循环。

## 4. healthz / 采样率 / 无 192k

```bash
curl -sS -D- -o /dev/null http://127.0.0.1:80/healthz | head -n 1        # 期望 HTTP/1.1 200 OK（daemon 在 :80；8090 是 rt-host 的 loopback probe 口，外部不通）
journalctl -b -u khaen-rt-host.service --no-pager | grep -Ei 'rate|48000|44100|96000|192000' | tail -n 10
```

期望 200；采样率保持 48k，**不得出现 192000/176400**（用户硬禁令）。

## 5. 无旧 C（G-no-C）

```bash
systemctl is-enabled tspi-camilladsp.service tspi-upmix-autodetect.service khaen-audio-web.service 2>/dev/null
pgrep -af 'camilladsp|tspi-upmix' || echo 'no C process (good)'
ls -l /run/tspi/camilladsp.yml /run/tspi/camilladsp.env /etc/tspi/last-mode 2>/dev/null || echo 'no C entry files (good)'
```

期望：camilladsp/upmix = **masked**（或 disabled），无 C 进程、无 C 激活入口文件。**不得**用任何 C 入口做回退。

## 6. last-good

```bash
ls -l /var/lib/tspi-rt-stack/last-good.json
python3 -m json.tool /var/lib/tspi-rt-stack/last-good.json | head -n 30
```

期望 schema `tspi-rt-stack/last-good/v1`、`c_policy=never-camilladsp`、release_id 与当前 running 一致；
last-good 只在 06 全 PASS 后由 07 原子写入——重启不会改写它。

## 7. 全量复核（只读；写 verify-report.json，不改运行态）

```bash
EV=/var/lib/tspi-rt-stack/evidence/$(date +%Y%m%dT%H%M%S)-bootverify
sudo /var/tmp/tspi-rt-stack-deploy/deploy/bin/06-verify-switch.sh --live \
  --require-daemon --require-health --settle-secs 3 --evidence "$EV"
echo "verify_rc=$?"; python3 -m json.tool "$EV/verify-report.json" | tail -n 20
```

期望 `verify_rc=0`、`fails=0`、`soft=0`，且 V1/V2/V10 全 PASS（V10.irqbalance / V10.unit / **V10.bootwait** /
**V10.legacy-dropin** / V10.pins）。仅当全 PASS 且需要更新 last-good 时，才在授权窗口内执行
`07-mark-last-good.sh --live` **[WRITE]**。

## 8. 判定表（症状 → 首查 → 处置）

| 症状 | 首查 | 处置（窗口内） |
| --- | --- | --- |
| host inactive，irq-affinity **failed**，日志 wait 超时 FAIL IRQ56/57 | `journalctl -b -u tspi-irq-affinity` | IRQ 号变化/设备未枚举：改 `/etc/tspi-rt-stack/irq-affinity.env` 的 pins 或加大 `TSPI_IRQ_WAIT_SECS` 后 `systemctl restart tspi-irq-affinity.service` **[WRITE]**；**不要**绕过 Requires= |
| irq-affinity active 但 host inactive | `journalctl -b -u khaen-rt-host`、`06 tarball` | 缺 toml/env、ALSA 被占、RT 门槛失败 → 按 01/06 门修配置；仍不行则 `08 --to-last-good` |
| target active、**daemon inactive、healthz 000** | B2 已知（`08 --mute` 只拉 host） | `systemctl start tspi-rt-stack.target` **[WRITE]**（host 已 mute，仅补 daemon）；终验 19/19 |
| IRQ 回到 7/3/5 | `systemctl is-enabled/ is-active irqbalance` | irqbalance 被 enable → `systemctl disable --now irqbalance` **[WRITE]** + `systemctl restart tspi-irq-affinity.service` |
| 发现 legacy `10-wait-irq.conf` | `§2` 命令 | 重跑 04（自动迁移到 evidence/migrated-dropins/）或删除后 `daemon-reload` **[WRITE]** |
| 反复重启仍失败 | — | **停止反复重启**；先按 §9 收集证据，报告 lead；不得恢复 C |

## 9. 失败取证（板恢复网络后先做，不改变状态）

```bash
TS=$(date +%Y%m%dT%H%M%S); OUT=/var/tmp/tspi-bootdiag-$TS; mkdir -p "$OUT"
{
  date; uname -a; uptime
  systemctl status tspi-irq-affinity.service khaen-rt-host.service tspi-rt-daemon.service tspi-rt-stack.target --no-pager -l
  systemctl cat tspi-irq-affinity.service
  ls -la /etc/systemd/system/tspi-irq-affinity.service.d/ 2>/dev/null
  for i in 58 56 57; do printf 'IRQ%s=' "$i"; cat /proc/irq/$i/smp_affinity_list 2>&1; done
  systemctl is-enabled irqbalance; systemctl is-active irqbalance
  journalctl -b -u tspi-irq-affinity.service -u khaen-rt-host.service --no-pager | tail -n 400
  cat /var/lib/tspi-rt-stack/last-good.json 2>/dev/null
} > "$OUT/bootdiag.txt" 2>&1
tar czf "/var/tmp/tspi-bootdiag-$TS.tgz" -C /var/tmp "tspi-bootdiag-$TS"
sha256sum "/var/tmp/tspi-bootdiag-$TS.tgz"
```

## 10. 回退（仅在授权窗口内；C 永不恢复）

```bash
sudo ./deploy/bin/08-rollback.sh --live --authorize /var/tmp/window.json --to-last-good
# 无 last-good 时：--mute → exit 10 DEGRADED_MUTE；随后按 §8 第 3 行补 start target 以恢复 daemon/healthz
```

回退判定：08 只允许自研 last-good 或 mute；C 保持 masked；若 mute 后 daemon/healthz 缺失，按 B2 已知项补
`systemctl start tspi-rt-stack.target` 后再跑 §7 终验。
