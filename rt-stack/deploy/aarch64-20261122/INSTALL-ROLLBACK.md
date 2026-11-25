# 安装 / 回滚说明（bundle aarch64-20261122）

> **状态声明：以下板端步骤全部 NOT_RUN。** 本 bundle 在 builder VM 内构建并离线自检通过，
> 但从未在泰山派 3M（192.0.2.215，RK3576 / Debian 12 / glibc 2.36）上安装、启动或出声；
> 板端窗口需另行授权（见 `docs/BOARD-WINDOW-RUNBOOK-T67-T74.md`，当前 PLAN-ONLY）。
> 权威 runbook 仍是 `rt-stack/deploy/README.md` + `rt-stack/deploy/REBOOT-RECOVERY-CHECKLIST.md`，本文件只针对本 bundle。

## 0. bundle 内容

| 路径 | 内容 |
| --- | --- |
| `bin/` | `khaen-rt-host`（1,451,984 B）、`tspi-cal`（2,608,352 B）、`tspi-rt-daemon`（3,171,080 B）—— 全部 ELF64 aarch64 PIE，GLIBC max 2.34 |
| `units/` | `khaen-rt-host.service`、`tspi-rt-daemon.service`、`tspi-irq-affinity.service`（含 B1 `ExecStartPre=...wait`）、`tspi-rt-stack.target`、`90-safe-mute.{conf,env}.example` |
| `etc/` | `rt-host.{env,toml}.example`、`rt-daemon.{env,toml}.example`、`rt-host-nsp7.{env,toml}.example`（MOTU/NSP7 形态，复用 motu-meter-dsp 部署样例）、`irq-affinity.env.example` |
| `manifest.json` | schema `tspi-rt-stack/release/v1`，逐文件 size/sha256；17 文件 |
| `SHA256SUMS` | 18 项（含 manifest.json 自身） |
| `README.deploy.md` | 打包器生成的摘要（指向 deploy/README.md） |
| `dist/` | 与目录同构的 tgz：`tspi-aarch64-20261122-deploy.tgz`（3,017,527 B，sha256 `c3218eb2…6fa6e5`） |

> 注意：`bin/` 与 `dist/*.tgz` 不提交 git（repo-hygiene）；交付副本见 `artifacts/audio/tspi-aarch64-20261122/`。

## 1. 上板前校验（只读，**板端 NOT_RUN**）

```bash
# 解包（若用 tgz）
sudo mkdir -p /var/tmp/tspi-rt-stack-release-aarch64-20261122
sudo tar -xzf tspi-aarch64-20261122-deploy.tgz -C /var/tmp/tspi-rt-stack-release-aarch64-20261122

cd /var/tmp/tspi-rt-stack-release-aarch64-20261122
sha256sum -c SHA256SUMS                       # 18 项全 OK
python3 <repo>/rt-stack/deploy/lib/release_manifest.py verify . --require-arch aarch64
#   → ok=true, files_checked=17, binaries=[bin/khaen-rt-host, bin/tspi-rt-daemon, bin/tspi-cal]

# 目标板 ABI 自检（板端 glibc 2.36 ≥ 产物 2.34）
file bin/*; ldd bin/khaen-rt-host | grep -E "libasound|libc"
systemctl is-active khaen-rt-host.service tspi-rt-daemon.service  # 记录现状（只读）
```

## 2. 安装（**NOT_RUN**；需授权窗口 `WINDOW.json`）

```bash
# 04 只落盘 + daemon-reload + enable（不 start）；IRQ 亲和 unit 走 B1 前置；不动旧 C 运行态
sudo bash <repo>/rt-stack/deploy/bin/04-install-release.sh --live \
  --authorize /var/tmp/WINDOW.json --bundle /var/tmp/tspi-rt-stack-release-aarch64-20261122
```

安装布局（脚本决定）：`/opt/tspi-rt-stack/releases/aarch64-20261122/{bin,units,manifest.json}`，
`/opt/tspi-rt-stack/current` 软链；units 落 `/etc/systemd/system/`；配置模板仅在缺失时复制到 `/etc/tspi-rt-stack/`
（`rt-host.env`/`rt-host.toml`/`rt-daemon.env`/`rt-daemon.toml`/`irq-affinity.env`）；
`/usr/local/lib/tspi/irq-affinity-pin` + disable irqbalance。

## 3. 切换与验证（**NOT_RUN**）

```bash
sudo bash <repo>/rt-stack/deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/WINDOW.json \
  --bundle /var/tmp/tspi-rt-stack-release-aarch64-20261122
sudo bash <repo>/rt-stack/deploy/bin/06-verify-switch.sh --live --authorize /var/tmp/WINDOW.json
sudo bash <repo>/rt-stack/deploy/bin/07-mark-last-good.sh --live --authorize /var/tmp/WINDOW.json
```

重启持久性（B1 IRQ 竞态必查）：按 `rt-stack/deploy/REBOOT-RECOVERY-CHECKLIST.md` §0–§6 执行后再标 last-good。

## 4. 回滚（**NOT_RUN**）

```bash
# 回到自研 last-good（失败自动降级 mute；P1 硬约束：C 永不作为回退目标）
sudo bash <repo>/rt-stack/deploy/bin/08-rollback.sh --live --authorize /var/tmp/WINDOW.json --to-last-good
# 更早一版自研 release
sudo bash <repo>/rt-stack/deploy/bin/08-rollback.sh --live --authorize /var/tmp/WINDOW.json --to-previous
# 只做保护静音（不切 release）
sudo bash <repo>/rt-stack/deploy/bin/08-rollback.sh --live --authorize /var/tmp/WINDOW.json --mute
```

退出码：0=已回 last-good；10=降级 mute（服务在、静音）；6=mute/停止且需人工（C 保持 masked）；
4=授权失败；5=状态冲突。**05/06/08 都不允许把 CamillaDSP/旧 C unit 当回退。**

## 5. tspi-cal（校准 CLI）用法

`tspi-cal` 是**离线校准链 CLI**（job/sweep/fit/compile/apply），不是常驻服务，无 systemd unit。
产物在 `--out DIR` 下（jobs/ + job.json + summary.json）。

```bash
# 离线合成 dry-run（fake 引擎；不碰声卡、不出声）——VM 内已跑通 exit 0
bin/tspi-cal run --design synthetic --out /var/tmp/cal-dryrun

# 走 apply（T66 commit + offline all-ack 模型；仍不碰声卡）
bin/tspi-cal run --design synthetic --out /var/tmp/cal-apply --apply

# 真实测量（需授权窗口 + 声卡 + 可闻扫频声明；NOT_RUN）
bin/tspi-cal run --engine tools --audible-authorized --out /var/tmp/cal-real --apply
```

注意（来自 CLI 自身说明）：apply-host 只是进程内 plan-arena 加载/swap，宿主机常驻播放路径尚不消费
calibration.yml/plan，**T3「校正已生效」= NOT_CONFIRMED**；`board_run`/`t3_applied_measurement` 均标 NOT_RUN。

## 6. 禁止事项 / 边界

- 不得把本 bundle 当作「已在板上验证」：板端安装/启动/重启/真实音频全部 **NOT_RUN**；
- 不得用本 bundle 覆盖/回退到旧 C（CamillaDSP/upmix）——P1 G-no-C 硬约束；
- `tspi-cal --engine tools` 会驱动真实声卡出声，必须在授权窗口 + `--audible-authorized` 下才允许；
- 本 bundle 由 clean HEAD `5c8ea11` 构建；若 HEAD 已前进，需重打（`BUILD-INFO.md` §6）。
