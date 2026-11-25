# T28 deploy —— 自研 RT-first DSP 栈 部署 / 原子切换 / 回退工具集

> WBS **T28**（task-146，release-owner）+ 扩展 task-147（aarch64 部署产物准备）。
> **状态（2026-11-18 F1–F5 修复后）：** 本地 `bash -n` **34/34**、dry-run **26/26 PASS**、unit/rollback 静态检查 exit 0
> （task-217 证据 `evidence/cursor-fix-deploy-f1f5/`；task-192 证据 `evidence/cursor-fix-irqpersist/`；修复轮证据 `evidence/cursor-fix-deploy/`；
> migration-plan 旧 20/20 证据为点-in-time，已不可复现，见 §6）；aarch64 双 bin 已按 M4 冻结构建并通过 qemu/打包校验；
> **板端部署/安装/播放仍 NOT_RUN**。本目录准备 T71 的脚本与计划，**不实际切板**：未停 S1、未改运行中服务、未连板、未 git。
>
> 2026-11-17 修复（Cursor F-17/F-18/F-40/F-41/F-42/F-43/F-45 + T28 评审 P0/P1/P2）：
> target 增 Wants/After host+daemon；04 安装 `rt-host.toml`/`rt-daemon.toml`（不覆盖）；05 在 install 前加只读
> preflight、start target 后逐个校验 host+daemon active、last-good 写失败用独立退出码 8；safe-mute 改用
> `TSPI_HOST_SAFE_MODE` + `EnvironmentFile` 覆盖 normal；`assert_c_retired` 要求 masked、V5 查 80 端口；
> preflight 192k 扫 toml 并拒 176.4k、udev 缺失硬失败；`ev_cmd` 传播非零。
>
> **2026-11-17 task-192（T71 前置，IRQ 亲和持久化入 deploy）**：task-189 板端实测 IRQ58/56/57 迁
> A53 后 T45 plan accepted，但 irqbalance 会在 ≤25s 内搬回 A72。本轮把持久化做进 deploy：
> ① `04-install-release.sh` 安装时 **disable+stop irqbalance**（记录原状态）并立即写
> IRQ58→CPU1/IRQ56→CPU2/IRQ57→CPU3 后回读校验；② 新增 `service/tspi-irq-affinity.service`
> （oneshot，`Before=khaen-rt-host.service`，host `Requires=` 它），每次 host 启动前重新钉核；
> ③ `01-preflight-switch.sh` 增 **J 门**、`06-verify-switch.sh` 增 **V10 门**（irqbalance 未复活 +
> IRQ 仍在 A53 + T45 IRQ 放置前置）。依据 task-189 实测 `evidence/board-irq-affinity/`；
> 本地 dry-run 22/22 PASS，**板端 IRQ 持久化执行 NOT_RUN**，证据 `evidence/cursor-fix-irqpersist/`。
> **2026-11-18 板窗 F1–F5 修复（task-217；证据 `evidence/cursor-fix-deploy-f1f5/`，重打包 `evidence/release-v4/`）**：
> ① **F1** 上传包脚本执行位：打包由 `bin/make-deploy-package.sh` + `lib/release_tar.py` 显式写 0755（不依赖宿主 chmod）并做
> 归档成员模式 + SHA256SUMS 包后校验（板端仍给 chmod 兜底）；② **F2** `03-retire-c.sh` 对 /etc/systemd/system 下普通 unit 文件
> 先备份到 evidence 再移走，然后 `systemctl mask`；③ **F3** host/daemon unit 增 `RuntimeDirectory`/`LogsDirectory=tspi-rt-stack`
> （/run、/var/log 目录在 mount namespace 建立前由 systemd 创建）；④ **F4** host `DeviceAllow=char-alsa rw`
> （systemd 252 + cgroup v2 下 `/dev/snd/*` 通配不生效）；⑤ **F5** `08-rollback.sh --mute` 生效核对改读
> `/proc/<MainPID>/environ`，回退解析 EnvironmentFiles（旧 `systemctl show -p Environment` 不含 EnvironmentFile，板端假阴性）。
> 本地 dry-run **26/26 PASS**（新增 `deploy_package_exec` / `retire_mask_unit_file` / `env_resolve` / `mute_env_runtime` 4 例）。
>
> **2026-11-18 release-v5（task-218；证据 `evidence/release-v5/`）**：按 **git ddeee04**（T45 真写内核 + F1–F5 修复）在 builder VM
> 重打 aarch64 host/daemon/web 并重打 bundle `t45-ddeee04-20261118-v5`（14 files；tgz 1,050,866 B `6ea0f326…87c6`）+ 板端上传包；
> **host 二进制因 T45 变化**（1,198,400 B `e6f07776…48522`；qemu 下 `kernel_applied=true/SCHED_FIFO/main_cpu=0/workers 2-5`），daemon 与 web
> 重编译字节同上批（`804a8bcd…`/`d458ae7b…`）；`etc/rt-host.env.example` 增 `TSPI_HOST_APPLY_AFFINITY=true` T45 开关示例（板窗需在已装 env 追加）。
> 上传包 `tspi-deploy-t45-ddeee04-20261118-v5.tgz` 构建机 Linux 解包 `sha256sum -c`+`test -x` PASS；板端全部 **NOT_RUN**。

## 1. 安全模型（硬约束）

| 门 | 规则 |
| --- | --- |
| G1 默认 dry-run | 所有变更脚本默认只打印命令；必须显式 `--live` 才执行（`run()` 包装，dry-run 下不落盘/不调 systemctl 变更） |
| G2 授权窗口 | live 必须 `--authorize <窗口 JSON>`；`lib/check_window.py` 校验 schema/时间/scope/`audio_off_required`/`s1_stop_acknowledged`/`soak_absent`/≤120min；缺失/过期/越权 exit 4 |
| G3 先只读 | 任何变更前先跑快照 + preflight + 设备独占；G3 失败 = 零变更（旧 C 原状） |
| G4 退役后不可回 C | `03-retire-c.sh` 之后失败只允许 `08-rollback.sh` 回自研 last-good 或 mute；脚本内含 `never-camilladsp` 契约，独立检查器 `tests/check_rollback_file.py` 会拒收任何 C fallback |
| G5 证据 | 每步写 `/var/lib/tspi-rt-stack/evidence/<ts>-<step>/`：命令/exit、JSON 报告、快照、验证结果 |
| G6 设备独占 | `02-check-device-exclusive.sh` 在 retire 后再验一次：旧 unit inactive、无 C 进程、`/dev/snd` 无其它持有者、PCM 全 closed、旧控制端口空 |

禁止：启动/使能/unmask CamillaDSP 或 `tspi-upmix-autodetect`；`tspi-mode dante|aes67`；改内核 cmdline/`rt_runtime`/CueMix；
未经授权窗口执行任何 live 命令；把 C 配置成 P1 fallback（DSP-HOST-DECISION §7 G-no-C）。

## 2. 目录

```text
deploy/
├── README.md                     本文件
├── lib/common.sh                 公共库：日志/run 包装/授权/证据/unit 查询/current release（symlink 或 pointer 降级）
├── lib/check_window.py           授权窗口校验（live 门）
├── lib/check_elf.py              ELF/PE 架构检查（aarch64 强制，防 PE/错架构冒充）
├── lib/release_manifest.py       release bundle 清单校验（schema/files/size/sha256/arch）
├── lib/last_good.py              last-good 记录校验（c_policy=never-camilladsp）
├── lib/snapshot_to_json.py       旧 C 快照 TSV+raw -> snapshot.json
├── lib/irq_affinity.py           IRQ 亲和/T45 IRQ 放置前置检查器（task-192；JSON 证据；非 plan() 本体）
├── config/old-c-units.conf       旧 C/旧 shim/AoIP 保留 unit 分类 + 入口文件 + 进程模式
├── config/rt-host.env.example    新 host 环境模板（T155 冻结；04 首次安装，不覆盖）
├── config/rt-daemon.env.example  新 daemon 环境模板（T155 冻结；04 首次安装，不覆盖）
├── config/rt-host.toml.example  新 host 配置模板（T155 冻结；F-18：04 必须安装，缺则 host 启动即退）
├── config/rt-daemon.toml.example  新 daemon 配置模板（同上）
├── config/switch-window.example.json  授权窗口示例（**示例不是授权**）
├── config/irq-affinity-pin        IRQ 钉核脚本（task-192；写 /proc/irq/*/smp_affinity_list 并回读；oneshot ExecStart）
├── config/irq-affinity.env.example IRQ 钉核配置模板（pins 58:1/56:2/57:3、T45 map、DSP_CPUS；04 首次安装不覆盖）
├── config/motu-48k-keeper          MOTU 48k keeper v2（两阶段：ALSA 空闲 1s 静音 + CueMix 只读设备速率；禁止 192k）
├── config/motu-rate-guard.env      keeper 开关（WRITE_BACK 默认 0；写 CueMix kSampleRate 未证）
├── config/tspi-motu-48k-keeper.{service,timer}  开机先置 48k + 60s 周期兜底
├── config/95-tspi-motu-48k-keeper.rules  USB 重枚举后重新置 48k
├── bin/00-snapshot-old-c.sh      旧 C 二进制/库/config/unit/进程/设备/运行态只读快照 + hash 清单
├── bin/01-preflight-switch.sh    切换前置门（两段：--pre-install 只读零变更门 / 完整 A–J：release/arch/unit/toml/audio-off/设备/last-good/192k/IRQ 持久化）
├── bin/02-check-device-exclusive.sh  设备独占门（不打开 PCM）
├── bin/03-retire-c.sh            停用/禁用/mask 旧 C + 清除旧激活入口（不可逆回 C）
├── bin/04-install-release.sh     安装 release（bin/units/env+toml；enable 不 start；可 --no-enable；keeper/udev 失败硬失败）
├── bin/05-atomic-switch.sh       T71 唯一入口：00→01(pre)→04→01(full)→03→02→start(等 host+daemon active)→06→07，失败只回 last-good/mute
├── bin/06-verify-switch.sh       切换后验证（C 退役保持(masked)/独占/端口 1234+80/RT/health/MOTU keeper V9/IRQ 持久化 V10）；写 verify-report.json
├── bin/07-mark-last-good.sh      成功后记录自研 last-good（原子写）
├── bin/08-rollback.sh            回退 last-good / previous / mute（绝不 C）
├── bin/make-release-bundle.sh    把 aarch64 二进制+units+配置打成可校验 release bundle（T147；--tgz 用 release_tar.py 写显式模式）
├── bin/make-deploy-package.sh    组装板端上传包（deploy/ + service/ + bundle + README + SHA256SUMS）；F1：release_tar.py 显式 0755 + 包后校验
├── lib/release_tar.py            可复现 tar 打包/校验（显式成员模式；修复 Windows chmod 不影响 tar 模式的 F1 根因）
├── tools/build-aarch64.sh        aarch64 构建脚本（native / multiarch / zigbuild；默认 dry-run，如实报缺项）
└── tests/                        本地真实干跑（mock systemd 等）、unit 语法检查、rollback 契约检查、inventory 生成器
```

## 3.1 MOTU 采样率防线（48k，禁 192k/176.4k）

为什么必须有：MOTU UltraLite-mk5 的采样率是**设备自身状态**，空闲或 USB 重枚举后会回落到面板/上次速率
（2026-11-15 20:40 实测快照 `kSampleRate=192000`）。**192 kHz 下 TOSLink 带不动 PCM，光纤输入
（MOTU Optical In → USB capture ch12/13）必然锁不上**，用户侧表现就是"光纤进了没声音"。

三层防线各管一个窗口：

| 层 | 组件 | 覆盖窗口 |
|---|---|---|
| 1 | `config/motu-rate-guard.env` + keeper Stage B（CueMix WS 只读 `kSampleRate`） | 设备被 rt-host 占用、ALSA 开不了流时仍能**看见**速率；≠48000 记 WARN，06-verify 直接 FAIL |
| 2 | keeper Stage A（ALSA 空闲时灌 1s 48k 静音）+ 60s timer + udev 重枚举触发 | 空闲/插拔后的回落窗口 |
| 3 | rt-host 开流即 48k（hw_params 固定 48000） | 正常播放期间 |

`WRITE_BACK=1`（让 keeper 直接写 `kSampleRate`）**默认禁止**：CueMix 写该 property 是否被设备接受仍是
**NOT_YET_PROVEN**，须先在白天窗口做"读→写→回读"受控探针证实，再改 `/etc/tspi/motu-rate-guard.env`。
06-verify 的 `V9.keeper.period/run/device.rate` 是**可证伪**检查：keeper 没在跑、周期过长、设备速率漂了，
都是 FAIL（2026-11-20 之前的实现只会给 NOT_VERIFIED，等于没防线）。

**操作纪律：不要在 rt-host 运行时带电插拔 MOTU USB**——重枚举正是速率漂移与主机侧 USB/xhci 异常的触发点。
需要改接线时先 `systemctl stop khaen-rt-host`（或断电接线后上电），接好再起。

## 3.2 板级韧性：把"一挂 8 小时"变成"30 秒自恢复 + 留证据"

2026-11-20 08:59 板子失去响应（电源灯亮、官方 5V/3A、ping/ssh 全死），无任何可读证据，直到 17:46 人为断电才恢复（~8h47m）。
因此新增四件套（`04-install-release.sh` 安装，`06-verify-switch.sh` 的 V11 组校验）：

| 组件 | 文件 | 作用 |
|---|---|---|
| 硬件看门狗 | `config/10-tspi-watchdog.conf` | systemd `RuntimeWatchdogSec=30s`：PID1 都被拖住时 SoC 自动复位，不再等人拔电 |
| 崩溃即重启 | `config/99-tspi-resilience.conf` | `panic_on_oops=1`、`hardlockup_panic=1`、`softlockup_panic=1`、`kernel.panic=10`：锁死会 panic → ramoops 落盘 → 10s 后自动重启 |
| 崩溃证据归档 | `config/pstore-archive` + `tspi-pstore-archive.service` | 开机第一件事把 `/sys/fs/pstore`（dmesg-0/1 + console + pmsg，位于 DRAM 0x40110000）复制到 `/var/log/tspi/pstore/<ts>-<boot_id>/`；**空的也写 INDEX.txt**（"空"=上次没有 panic，本身是证据） |
| 温度/负载历史 | `config/thermal-log` + `tspi-thermal-log.timer` | 60s 一条：各 thermal_zone、loadavg、cpu freq、`sched_rt_runtime_us` → `/var/log/tspi/thermal.log` |

两条硬纪律：

1. **不要动 `sched_rt_runtime_us`**（保持 950000/1000000）。设成 -1 会让 SCHED_FIFO 线程在 bug 时完全饿死内核线程，表现正好是"网络/SSH 全死但板子还在跑"。x86 压测脚本里那条 `-1` 不要照搬到板子。
2. **下次再失联，先读 pstore 再断电**：ramoops 在 DRAM，冷断电会把它连同证据一起抹掉。

离线校验：`py -3 deploy/tools/check_resilience.py`（应 PASS 28 / FAIL 0）。

## 3. 典型 T71 流程（本任务不执行）

1. **准备（构建 VM/开发机）**：用 gnu 路径构建 aarch64 产物（T149 已产出 `evidence/deploy-artifacts/artifact/release-bundle/`；复现见 `evidence/deploy-artifacts/TOOLCHAIN.md`/`BUILD-GNU.md`）→
   `make-release-bundle.sh --live --bin-dir <target>/release --out <bundle> --release-id <id>`；
   把 `rt-stack/deploy/`、`rt-stack/service/`、bundle 传到板端 `/var/tmp/tspi-rt-stack-deploy/`。
   直接产出板端上传包：`make-deploy-package.sh --live --bundle <bundle> --out tspi-deploy-<id>.tgz`
   `--readme README-UPLOAD.md`（F1：包内脚本为 0755，打包后校验归档模式 + SHA256SUMS；板端若经中转丢位仍可
   `chmod +x deploy/bin/*.sh deploy/config/irq-affinity-pin deploy/config/motu-48k-keeper` 兜底）。
2. **授权**：用户/lead 出具窗口 JSON，时间覆盖操作全程，scope 至少含
   `snapshot,preflight,retire-c,install-release,start-new,verify,mark-last-good,rollback`，并确认 audio-off / S1 可停 / 无 soak。
3. **切换（板端 root，窗口内）**：
   ```bash
   sudo ./deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/window.json \
        --bundle /var/tmp/tspi-rt-stack-release-<id> --allow-first-switch
   ```
   首次切换无自研 last-good，`--allow-first-switch` 声明失败回退=mute；以后必须已有 last-good。
   **05 内部顺序**：00 snapshot → 01 `--pre-install`（失败=完全零变更）→ 04 install（落盘 release/current、装 env+toml、
   **disable+stop irqbalance 并写 IRQ pin**、enable keeper；不 start）→ 01 完整 A–J → 03 retire → 02 设备独占 →
   start keeper → **start tspi-irq-affinity.service（task-192 钉核；B1：ExecStartPre 先等 IRQ 节点就绪再钉核）** →
   enable/start target（**target Wants host+daemon，05 逐个等 host 和 daemon active**）→ 06 verify（含 V10 IRQ 持久化 +
   V10.bootwait/legacy-dropin）→ 07 last-good。
   **04 安装并 enable MOTU 48k keeper（boot service + 5min timer + udev 重枚举规则，失败硬失败）；05 在 start target 前先 start keeper**，
   保证新 host 打开 PCM 前设备已在 48k（用户禁止 192k）。
4. **失败**：05 自动调 08；最终状态见退出码。无论何种失败，**C 保持 disabled/masked**，不得人工恢复 C 作为 P1 fallback。
5. **证据**：05 的 evidence 目录含 stages.log/result.json + 各子步骤；T71/T72 引用它做 G-no-C 证据。

只读预演（任何时间可跑，不连板、不落盘）：

```bash
./deploy/bin/00-snapshot-old-c.sh --dry-run
./deploy/bin/05-atomic-switch.sh --dry-run --bundle /path/to/bundle
```

## 4. 退出码

| 码 | 含义 |
| --- | --- |
| 0 | 成功 / 已是目标状态 / preflight 门通过 |
| 2 | 用法错误、缺 bundle/依据文件 |
| 3 | 前置门/安装校验失败（**音频零变更**；install 之前失败=完全零变更，install 之后=release/current 已落盘但旧 C 运行态未动；T28-P2-3 口径） |
| 4 | 授权窗口缺失/过期/scope 不符 |
| 5 | 切换后验证失败或安装后启动失败；**已回滚到 last-good**（05 口径） |
| 6 | 回滚失败/静默停止（C 保持 masked，需人工）；或退役不完整（不启动新栈） |
| 7 | 环境不支持（非 root、缺命令） |
| 8 | 新栈已验证运行，但 last-good 记录失败（RUNNING_NO_LAST_GOOD；播放不中断，须人工补记；F-41 独立码） |
| 10 | 降级 mute（host 进程在但保护静音；C 仍 masked） |

## 5. 安装布局与 last-good

```text
/opt/tspi-rt-stack/releases/<id>/{bin,units,manifest.json,README.deploy.md}
/opt/tspi-rt-stack/current            Linux symlink（无符号链接权限的夹具降级为 current.release 指针文件）
/etc/systemd/system/tspi-rt-{host,daemon}.service + tspi-rt-stack.target
/etc/systemd/system/tspi-irq-affinity.service   task-192 IRQ 钉核 oneshot（Before=khaen-rt-host.service）
/etc/systemd/system/tspi-motu-48k-keeper.{service,timer} + /etc/udev/rules.d/95-tspi-motu-48k-keeper.rules
/usr/local/lib/tspi/motu-48k-keeper（1s 静音 48k；不碰 CueMix/Toslink 路由）
/usr/local/lib/tspi/irq-affinity-pin           task-192 钉核脚本（写 /proc/irq/<irq>/smp_affinity_list + 回读）
/etc/tspi-rt-stack/{rt-host.env,rt-daemon.env,rt-host.toml,rt-daemon.toml}
/etc/tspi-rt-stack/irq-affinity.env            task-192 钉核配置（TSPI_IRQ_PINS="58:1 56:2 57:3" 等）
/var/lib/tspi-rt-stack/last-good.json   自研 last-good（schema tspi-rt-stack/last-good/v1；c_policy=never-camilladsp）
/var/lib/tspi-rt-stack/evidence/<ts>-*/ 各步骤证据
```

last-good 只在 `06-verify-switch.sh` PASS 后由 `07-mark-last-good.sh` 原子写；回退只信 `last_good.py validate`
（schema + c_policy + release 目录/binary hash）。首次切换无 last-good，回退=mute。

### 5.1 IRQ 亲和持久化（task-192；T71 前置）

**为什么**：T45 `scheduler::plan()` 要求 worker 依赖 IRQ（bit0/1 USB、bit2 PTP、bit3 eth）不落在
DSP_CPUS={4,5,6,7}（A72）。task-189 板端实测（`evidence/board-irq-affinity/`）：把 IRQ58(xhci/MOTU)→CPU1、
IRQ56(end0/PTP)→CPU2、IRQ57(end0 第二队列)→CPU3 后 plan accepted（affinity_hash `7f1dda556b4b582b`）；
但 **irqbalance 重启 ≤25 s 内会搬回 A72**（58→7、56→3、57→5）→ 迁移不持久、重启后 T45 又会拒绝。

**默认 DSP_CPUS 已改 4-7（A72 全核；planrt-20261119）**：`rt-host.toml.example`/`rt-host.env.example` 默认 `dsp_cpus="4-7"`；旧默认 2-5 在 RK3576 上混用
A53(0-3)+A72(4-7)，实测 G-4096 仅 96.1 blocks/s，而 4-7 全 A72 为 314.8 blocks/s（3.1×）。IRQ 仍钉 A53，
与本节的 T45 IRQ 放置前置一致；板端已安装 env 不会被 04 覆盖（需窗口内显式改 `/etc/tspi-rt-stack/rt-host.env`）。

**方案（组合，选择理由）**：
- **a) 停 irqbalance**：`04-install-release.sh` 在安装阶段执行 `systemctl disable irqbalance` +
  `systemctl stop irqbalance`（等价 `disable --now`），并把原 enabled/active 写入
  `install-record.json` 供回滚/审计。理由：task-189 证明只写 /proc 会被 irqbalance 覆盖；
  disable 保证重启后不再自动均衡。
- **b) oneshot 钉核**：`service/tspi-irq-affinity.service`（`Type=oneshot`、`RemainAfterExit=yes`、
  `Before=khaen-rt-host.service`，`ExecStart=/usr/local/lib/tspi/irq-affinity-pin apply`）在每次 host
  启动前重新按 `/etc/tspi-rt-stack/irq-affinity.env` 写 `smp_affinity_list` 并回读
  `effective_affinity_list`；`khaen-rt-host.service` 用 `Requires=`/`After=` 依赖它——钉核失败则 host
  不启动（fail-safe，不用错误拓扑跑 admission）。理由：覆盖开机、手动 `systemctl enable --now irqbalance`、
  热插拔后 IRQ 号/亲和漂移；比只停 irqbalance 更能证明「host 启动瞬间 IRQ 已在 A53」。
- **b2) 开机竞态等待（B1；2026-11-20 板窗）**：v11 重启实测 oneshot 开机 ~4s 先跑，而 end0 的 IRQ56/57 节点由
  stmmac 稍后才注册 → `irq-affinity-pin apply` fail-closed，`khaen-rt-host.service` 因 `Requires=` 不启动
  （target/daemon active 但无音频 owner）。修复固化：oneshot 增加
  `ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait`（轮询 0.2s，上限 `TSPI_IRQ_WAIT_SECS` 默认 60s，
  0=只查一次；超时仍 fail-closed 不启动 host）与 `TimeoutStartSec=120`；`irq-affinity-pin` 新增 `wait` 动作
  （只读等待，不写 /proc）。升级迁移：04 安装时把板端 live-only 降级 drop-in `10-wait-irq.conf` 备份到
  `evidence/migrated-dropins/` 后删除（避免陈旧硬编码 IRQ 等待与新 unit 叠加）；安装前/后均硬校验 bundle
  的 unit 含该前置（旧 bundle 直接 exit 3，不改板）；`make-release-bundle.sh` 也会对打包的 units 跑
  `check_unit_syntax.py` 契约检查（含 B1 断言）。
- **c) 双重校验**：`01-preflight-switch.sh` **J 门**（完整模式）：irqbalance 未 enabled/active、
  oneshot unit + env 已安装、依赖 IRQ 的 `/proc/irq` 节点存在、当前 effective 已在 A53 且
  T45 IRQ 放置前置 PASS（`lib/irq_affinity.py`）；`06-verify-switch.sh` **V10 门**在 host 启动后复验
  同一组条件并写 `irq-affinity-check.json`。
- 说明：`lib/irq_affinity.py` 只校验 **plan 的 IRQ 放置维度**，不调用 `plan()` 本体；真实 plan accepted
  证据仍是 task-189 的 `t45-probe-*.json`。

**回滚（手动；本任务不改板端）**：

```bash
systemctl disable --now tspi-irq-affinity.service       # 不再开机钉核
systemctl enable --now irqbalance                      # 恢复自动均衡（会搬回 A72）
echo 7 > /proc/irq/58/smp_affinity_list                # 可选：恢复 task-189 前实测值
echo 6 > /proc/irq/56/smp_affinity_list
echo 4 > /proc/irq/57/smp_affinity_list
```

判定口径：板端执行前必须仍有合法窗口；回滚后 `01-preflight` J 门/T45 会重新拒绝，这是预期（T45 需要 A53
放置）。**不得**把「停 irqbalance」当长期永久方案（失去其它 IRQ 均衡）；长期由 irqbalance pin policy 或
T26 启动序列接管（task-189 REPORT §5）。

### 5.2 重启持久性 / 上电恢复检查清单（B1；2026-11-20）

板子每次重启后先跑只读检查（完整版含判定表/取证/回退：**`deploy/REBOOT-RECOVERY-CHECKLIST.md`**）：

```bash
systemctl is-active tspi-irq-affinity.service khaen-rt-host.service tspi-rt-daemon.service tspi-rt-stack.target
cat /proc/irq/58/smp_affinity_list /proc/irq/56/smp_affinity_list /proc/irq/57/smp_affinity_list   # 期望 1 2 3
curl -sS -o /dev/null -w 'healthz=%{http_code}\n' http://127.0.0.1:80/healthz                         # 期望 200（daemon 在 :80；8090 仅 loopback probe）
grep -c '^ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait$' /etc/systemd/system/tspi-irq-affinity.service  # 期望 1（v12+）
```

- host inactive 而 target/daemon active → **首查 irq-affinity**（`journalctl -b -u tspi-irq-affinity`）；
  wait 超时 = IRQ 号变化/设备未枚举，改 pins 或 `TSPI_IRQ_WAIT_SECS` 后重启 oneshot；**不得**用
  `systemctl start khaen-rt-host.service` 绕过 Requires= 前置。
- `08 --mute` 后 daemon/healthz 缺失是已知 B2（08 只拉 host）→ `systemctl start tspi-rt-stack.target` 补齐后终验。
- 全量只读复核：`06-verify-switch.sh --live --require-daemon --require-health`（含 V10.bootwait / V10.legacy-dropin）。

## 6. 本地验证（本任务真实执行）

```bash
bash deploy/tests/check_syntax.sh                 # bash -n 全部脚本+mocks（现行 34/34）
bash deploy/tests/run_dryrun_tests.sh --out ...   # 28 组真实 dry-run（含 irq_boot_wait/irq_boot_persist）
py deploy/tests/check_unit_syntax.py service      # service 6/6（缺省 service+deploy/config 7/7）；F3/F4/B1 断言
python deploy/lib/irq_affinity.py check --root <夹具>   # IRQ 亲和/T45 IRQ 放置前置（task-192；可用于任意只读复验）
python deploy/tests/check_unit_syntax.py service   # 在 rt-stack/ 下
python deploy/tests/check_rollback_file.py --deploy deploy --service service
```

结果与日志：
- 2026-11-20 **task-262（B1 开机 IRQ 竞态固化；证据 `evidence/irq-boot-persist/`）**：`service/tspi-irq-affinity.service` 增
  `ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait` + `TimeoutStartSec=120`；`deploy/config/irq-affinity-pin` 增
  `wait` 动作（默认 60s，`TSPI_IRQ_WAIT_SECS` 可调，0=只查一次）；04 安装前/后硬校验 + 迁移板端 legacy live-only
  drop-in（`10-wait-irq.conf` → `evidence/migrated-dropins/`）；01 J 增“开机竞态防护已安装/无遗留 drop-in”、
  06 V10 增 `V10.bootwait`/`V10.legacy-dropin` 门；`make-release-bundle.sh` 对 units 跑契约检查并支持 `--service-dir`。
  真跑 dry-run **28/28 PASS**（新增 `irq_boot_wait`、`irq_boot_persist`；含超时/晚到/迁移/负例）、`bash -n` **34/34**、
  unit 语法 **7/7**、rollback 契约 ok；**板端重启复验 NOT_RUN**（上电后按 `deploy/REBOOT-RECOVERY-CHECKLIST.md`
  执行并取证 V10.bootwait/legacy-dropin/19/19）。
- 2026-11-18 **task-223（v6b 包 / T72-N9 unit 依赖修复）**：`deploy/config/tspi-motu-48k-keeper.service` 的
  `Before=` 删除旧 C unit 名（只保留 `khaen-rt-host.service`，48k 先于 host 的排序语义不变）；
  `deploy/tests/check_unit_syntax.py` 缺省同时检查 `service/` 与 `deploy/config/`，并把 `khaen-audio-web`、
  `tspi-boot-mode` 加入旧 C 拒绝词（此前 keeper 不在缺省检查范围 → 6/6 PASS 却漏检）；unit 语法 **7/7**；
  v6b 上传包与本地/板端 before-after 证据见 `evidence/cursor-fix-unit-deps/`。
- 2026-11-18 **task-218（现行 release-v5）**：按 git ddeee04 在 builder VM 重打 aarch64 host/daemon/web、重打 bundle/上传包，
  构建+打包+qemu+Linux 解包校验证据见 `evidence/release-v5/`（tools/ 内含本次实际构建脚本）；本地回归沿用 task-217 的 26/26。
- 2026-11-18 **task-217（F1–F5 修复）**：`evidence/cursor-fix-deploy-f1f5/`——dry-run **26/26 PASS**（新增 F1 包执行位/
  F2 mask 前移走/F5 运行时环境/F5 mute 报告 4 例，mock systemd 默认按真实 Environment 语义），`bash -n` **34/34**、
  unit 语法 **6/6**（含 F3/F4 负例）；重打 `evidence/release-v4/`（bundle `m4-15cf411-20261118-v4` + 上传包）；
  制品见 FIX-REPORT.md（F1–F5 逐条 before/after）；板端复跑 NOT_RUN。
- 2026-11-17 **task-192（现行原文，已被上一行取代）**：`evidence/cursor-fix-irqpersist/`——dry-run **22/22 PASS**
  （新增 `irq_pin_script` / `irq_persist_regression` 用例、原 20 例加 task-192 断言）、`bash -n` 33/33、
  unit 语法 6/6、rollback 契约 exit 0；含 before/after diff、命令与 exit、板端 NOT_RUN 说明；
- 2026-11-17 修复轮：`evidence/cursor-fix-deploy/dryrun/`（20/20 PASS）、`.../syntax-check.log`、`.../unit-syntax.json`；
- 旧证据 `evidence/migration-plan/dryrun/`（2026-11-17 14:29 生成、标 20/20）是**点-in-time**：01/04/05/06 在 17:40–17:51 又改过，
  且 06 V9 keeper 门在夹具下查 `$MOCK_STATE/units` 而 04 把 keeper 装到夹具 `/etc/systemd/system` → 评审时实测 18/20。
  本轮修 mock 回落/夹具口径后真跑 20/20；旧日志不再代表现行脚本。详见 `evidence/cursor-fix-deploy/README.md`。

### 6.1 产物验证（Linux 构建机/板端）

```bash
bash deploy/tools/verify-artifact.sh <bin> --sysroot <aarch64 sysroot> --selftest --json <out>.json
# 检查 ELF aarch64 / NEEDED(libasound.so.2, libc.so.6) / GLIBC max<=2.36 / qemu selftest
```

**现行产物（task-218 / release-v5，git ddeee04：T45 真写内核 + F1–F5 deploy 修复）**：host 因 T45 重编译，daemon/web 源未变
（重编译字节同上批）—— 见 `rt-stack/evidence/release-v5/`：host **1,198,400 B / sha256 `e6f07776…48522`**、
daemon **1,286,384 B / sha256 `804a8bcd…577c18`**、web dist **4,519,296 B / sha256 `d458ae7b…d240`**；
bundle **`t45-ddeee04-20261118-v5`**（14 files；tgz 1,050,866 B `6ea0f326…87c6`；`release_manifest.py verify --require-arch aarch64` ok）。
**T71 板窗复跑使用该 bundle/上传包**：`evidence/release-v5/artifact/tspi-deploy-t45-ddeee04-20261118-v5.tgz`
（清单见 §6.2 与 `deploy/UPLOAD-MANIFEST-t45-ddeee04-20261118-v5.md`；包大小/sha 见 `evidence/release-v5/ARTIFACTS.md`）。
历史：M2b（task-194，29e16af：host 933,384 B `3a876e10…` / daemon 1,281,952 B `4fbfdd33…` / bundle `m2b-29e16af-20261117`）已被 M4 取代；T149 产物与部署清单见 `evidence/deploy-artifacts/artifact/` 与 `DEPLOY-CHECKLIST.md`；T155 接线产物与 bundle 见 `evidence/final-wiring/artifact/`（release_id `t155-final-20261117`，双 bin，manifest 11 files）——均被相继取代。
构建/同步/验证脚本：`deploy/tools/build-aarch64.sh --mode cross-sysroot`、`evidence/deploy-artifacts/tools/{sync-to-builder,build-on-builder}.sh`、`evidence/release-v3/tools/build-{rt,web}-on-builder.sh`（M4 实际执行）、`evidence/release-v5/tools/build-*-v5-on-builder.sh`（release-v5 实际执行）。

> **注意（2026-11-17 修复轮 + task-194 + task-214 + task-217 + task-218）**：T155 冻结 bundle 早于 F-17/F-18/F-43 修复——bundle 内 target 无 Wants=host/daemon、
> safe-mute drop-in 仍写 `TSPI_RT_HOST_SAFE_MODE`、04 也不会装 toml。task-194（M2b）已用现行 `service/`+`deploy/` 重打
> `m2b-29e16af-20261117`；task-214 已按 M4（git 15cf411）重打 `m4-15cf411-20261118`（M4 rt-host content_map/control 修复入包；
> units/etc 与 M2b 同字节；见 `evidence/release-v3/`）；**task-217（2026-11-18）按板窗 F1–F5 重打
> `m4-15cf411-20261118-v4`**（F3/F4/F5 修改 units，F1/F2/F5 修改 deploy 脚本；binaries/etc 不变；见 `evidence/release-v4/`）；
> **task-218（2026-11-18）按 git ddeee04（T45 真写内核）重打 `t45-ddeee04-20261118-v5`**（host 因 T45 变化，daemon/web 重编译字节同上批；
> `etc/rt-host.env.example` 增 T45 开关示例；见 `evidence/release-v5/`）——**T71 板窗复跑使用该 bundle**；
> 上传清单见 `deploy/UPLOAD-MANIFEST-t45-ddeee04-20261118-v5.md`。
> T149/T155/M2b/release-v3 仅历史；本轮没有改冻结的 T149/T155/release-v2/release-v3 证据目录。

**P1 发布包（T31/T32，task-162）**：产物 sha 清单/验收矩阵/证据索引见 `evidence/release-v1/`（RELEASE-NOTES.md / ACCEPTANCE-MATRIX.md / ARTIFACTS.md / EVIDENCE-INDEX.md）；M2b 产物重打包包（task-194）见 `evidence/release-v2/`；**M4 产物重打包包（task-214）见 `evidence/release-v3/`**；**F1–F5 修复重打包（task-217）见 `evidence/release-v4/`**（release notes/artifacts/not-run/builder 日志/上传包）；**T45 重打 release-v5（task-218，现行）见 `evidence/release-v5/`**。
web aarch64 dist 现行 = **release-v5（task-218，ddeee04 树重打）4,519,296 B，sha256 `d458ae7b90d683ad541946a9e6f761918fdd4f5af06dcd8e177842735d95d240`**（web 源码自 29e16af 未变，与 M2b/M4 字节相同；qemu mock 实测 `/api/audio/format` limits 4 档、PUT 192000→400、PUT 176400→400、PUT 96000→200；见 `web/dist/INSTALL-MANIFEST.md`、`web/dist/verify.log` 与 `evidence/release-v5/logs/web-rebuild/`）；**未上板部署**，板端仍是旧 `83ee5da2…`（运行期仍接受 192k，部署前必须替换）。

### 6.2 板端上传包（release-v5 / task-218，现行）

- 上传包：`rt-stack/evidence/release-v5/artifact/tspi-deploy-t45-ddeee04-20261118-v5.tgz` = `deploy/`（含 F1–F5 修复脚本 + T45 env 开关示例 + `make-deploy-package.sh`/`release_tar.py`）+ `service/` + `release-bundle/`（**T45 新 host**）+ `README-UPLOAD.md` + 包级 `SHA256SUMS`。
  **包大小/sha256/逐文件清单**见 `rt-stack/evidence/release-v5/ARTIFACTS.md`（包内文档不写自身 sha，避免自引用；包外有 `.sha256`）。
- **F1 执行位**：`make-deploy-package.sh` 在构建机（Linux）用 `release_tar.py` 显式写 0755 后打包，并做了归档成员模式校验 + 解包 `sha256sum -c`/`test -x`；板端 §3 仍保留 `test -x` 自检与 `chmod +x` 兜底。
- **T45/F6 板窗必做**：已装板 `/etc/tspi-rt-stack/rt-host.env` 不被 04 覆盖 → 手动追加 `TSPI_HOST_APPLY_AFFINITY=true` 后重启 host，并按 host JSON 核对 `scheduler.kernel_applied=true`（失败 fail-closed）。
- 上传/校验/切换命令、板端目标路径、关键文件 sha：见 `deploy/UPLOAD-MANIFEST-t45-ddeee04-20261118-v5.md`；板端只读校验 `sha256sum -c SHA256SUMS` + `release_manifest.py verify release-bundle --require-arch aarch64`。
- 板端安装/启动/回滚演练 **NOT_RUN**（T71 授权窗口执行；G-no-C 不变）；F3/F4 的 systemd 语义须板端复跑最终确认。

### 6.3 2026-11-18 板窗 F1–F5 修复（task-217）

| 缺陷 | 根因 | 修复 | 回归 |
| --- | --- | --- | --- |
| F1 上传包脚本无执行位（0664，05 首次运行 rc=126） | Windows 构建机 `chmod` 不改变 tar 模式；旧流程直接 `tar czf` | `bin/make-deploy-package.sh`：staging 后由 `lib/release_tar.py` **显式写成员模式**（脚本 0755/其余 0644），包后校验归档成员 exec 位 + SHA256SUMS；Linux 再解包 `sha256sum -c`/`test -x`；README §3 保留 chmod 兜底 | `deploy_package_exec`（归档模式断言 + 解包 sha 校验）；release-v4 构建机 Linux 解包校验 PASS |
| F2 03-retire 对 /etc/systemd/system 普通 unit 文件 mask 失败（exit 5/6） | `systemctl mask` 需在该路径建 /dev/null 链接，不能覆盖既有文件 | `unit_file_move_before_mask()`：普通文件先备份到 `evidence/retired-unit-files/`（+sha256、moved-unit-files.tsv）再移走，然后 mask；目录等异常 fail-closed | `retire_mask_unit_file`（mock mask 对既有普通文件失败 → 03 仍 rc=0 且证据齐全） |
| F3 host/daemon 缺 /run、/var/log 目录 → mount namespace 启动即崩 | `ProtectSystem=strict` + `ReadWritePaths` 要求目录在 namespace 建立前存在 | 两 unit 增 `RuntimeDirectory=tspi-rt-stack`/`LogsDirectory=tspi-rt-stack`（Mode=0755）；`check_unit_syntax.py` 断言 ReadWritePaths 被覆盖 | unit 语法 + 负例（删除 RuntimeDirectory 必 FAIL） |
| F4 `DeviceAllow=/dev/snd/* rw` 在 systemd 252+cgroup v2 不生效（ALSA 全 EPERM） | 设备白名单按节点匹配，通配不生效 | host unit 改 `DeviceAllow=char-alsa rw`（ALSA 设备组；daemon 不打开 PCM 不限制）；checker 拒绝 `/dev/snd` 通配 | unit 语法 + 负例（改回通配必 FAIL）；板端真 open 仍待复跑 |
| F5 08-rollback mute 假阴性 NOT_APPLIED | `systemctl show -p Environment` 不含 EnvironmentFile 内容，mute 经 drop-in EnvironmentFile 注入 | `lib/common.sh::unit_env_contains` 改：/proc/<MainPID>/environ → EnvironmentFiles 逐文件解析 → Environment 兜底；报告写明核对通道 | `env_resolve`（主通道/回退/服务在但无 mute 三态）+ `mute_env_runtime`（08 报告 已生效+/proc/<pid>） |

## 7. 当前 NOT_RUN / 阻塞（不得据此声称可用）

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 板端执行（T71） | **NOT_RUN** | 本任务不连板、不停 S1、不改运行中服务 |
| aarch64 `khaen-rt-host`（gnu） | **BUILT（task-218 v5，现行；T45 真写内核）** | builder VM + Debian12 aarch64 sysroot 交叉（git ddeee04 重编译）：**1,198,400 B，sha256 `e6f07776f58c34492b05c934bd04f9b6c00e8c99b8f59f41a6b1655783486522`**；ELF aarch64、NEEDED libasound.so.2/libgcc_s/libc、GLIBC max 2.34；qemu selftest/admission-check/fake-io/run(fake) PASS + **T45 probe（sudo）`kernel_applied=true/SCHED_FIFO/mlockall/main_cpu=0/workers 2-5`；非 root fail-closed rc=1**。见 `evidence/release-v5/`（release-v3 M4 的 1,176,128 B `fb247696…` 已被取代） |
| aarch64 `tspi-rt-daemon`（gnu） | **BUILT（task-218 v5 重编译，字节同 M4）** | **1,286,384 B，sha256 `804a8bcd94bc73cf8aff29bc459633abbee0cf55d50c512ad4215c410c577c18`**（T45 未改 daemon 源，重编译字节相同）；qemu `check --config` + `serve`+/healthz 200/优雅退出 PASS；release bundle `t45-ddeee04-20261118-v5` 含双 bin（14 files / --require-arch aarch64 ok；F3/F4 修复 units）；M2b 的 1,281,952 B `4fbfdd33…` 已被取代 |
| aarch64-musl 构建 | **NOT_BUILT** | T147 本机真尝试在 alsa-sys 处 exit 101（缺 aarch64 libasound sysroot/pkg-config）；板端 Debian 12 是 glibc，生产走 gnu 路径 |
| aarch64 `khaen-audio-web` dist（gnu） | **BUILT（task-218 v5 重打，字节同 M4，未上板）** | **4,519,296 B，sha256 `d458ae7b90d683ad541946a9e6f761918fdd4f5af06dcd8e177842735d95d240`**（ddeee04 树重打，web 源码自 29e16af 未变→与 M2b/M4 同字节）；源码 4 档白名单 [44.1/48/88.2/96k] + 176.4/192k 黑名单；cargo test 168/168；qemu mock healthz 200、PUT 192000→400、PUT 176400→400、PUT 96000→200。见 `web/dist/INSTALL-MANIFEST.md`、`evidence/release-v5/logs/web-rebuild/`（task-162 的 4,535,680 B `502ae949…` 已被取代） |
| P1 发布包（T31/T32） | **PACKAGED（候选，NOT ACCEPTED）** | `evidence/release-v1/`：release notes/验收矩阵/证据索引/产物 sha；P1 板端门 T67–T69/T71/T72/T74 NOT_RUN；终审 v0.9 P0/P1 未关闭；性能门后移 P4 |
| host 生产 CLI `run --config` | **WIRED（T155）** | `khaen-rt-host run --config rt-host.toml`：建图 / ALSA owner 契约(fake) / 4 worker DAG + T45 调度计划 / T20 swap(all-ack) / T21 fault 事件+watchdog / Web 端口接线(external 或 spawn 探测 /healthz 200) / 优雅退出(SIGINT/SIGTERM 或 --blocks/--duration-ms)。配置 schema `khaen-rt-host/v1` 见 `deploy/config/rt-host.toml.example` |
| daemon `serve --config` | **WIRED（T155）** | `tspi-rt-daemon serve --config rt-daemon.toml`：T42 REST + T44 modes + T70 Web/WS 同端口；`/healthz` 200、`/` 8656 B 页面；`--exit-after-ms`/SIGTERM 优雅退出。schema `tspi-rt-daemon/v1` 见 `deploy/config/rt-daemon.toml.example` |
| safe-mute 变量（F-17） | **WIRED（T155 + 2026-11-17 修复）** | host 读 `TSPI_HOST_SAFE_MODE=normal|mute`；drop-in 经 `EnvironmentFile=90-safe-mute.env` 覆盖 rt-host.env 的 normal（systemd.exec：EnvironmentFile 覆盖 Environment=）；08-rollback 安装后核对 `/proc/<MainPID>/environ` 含 mute（回退解析 EnvironmentFiles；F5，2026-11-18） |
| 新栈 target Wants / toml 安装（F-18/T28-P0） | **WIRED（2026-11-17 修复；release-v4 已重打）** | `tspi-rt-stack.target` Wants/After host+daemon；04 首次安装 `rt-host.toml`/`rt-daemon.toml`（不覆盖）+ env；01/06 把配置存在性当硬门；05 start 后等 host+daemon active |
| 真实 systemd 语法验证 | **NOT_RUN** | 本机无 systemd；用 `tests/check_unit_syntax.py` 静态检查替代，板端仍需 `systemd-analyze verify` |
| 板端 IRQ 亲和持久化（task-192） | **NOT_RUN** | 未上板；irqbalance disable/stop、写 /proc/irq 亲和、oneshot 与 host 的启动顺序只在本机 mock 夹具验证（现行 28/28）。板端仍须在 T71 授权窗口内执行，并由 preflight J / verify V10 取证（task-189 已实测 IRQ 迁移本身可逆可用） |
| 板端重启持久性（B1 开机 IRQ 竞态，2026-11-20） | **FIXED-IN-REPO / 板端 NOT_RUN** | v11 板窗首启 FAIL：oneshot ~4s 先于 end0 IRQ56/57 节点就绪 → apply fail-closed、host 因 Requires= 不启动；现场 live-only drop-in 修复后二次重启 PASS。task-262 已固化（unit wait 前置 + pin `wait` 动作 + 04 安装门/legacy drop-in 迁移 + 01 J / 06 V10 回归门 + bundle 打包门）；本机 dry-run 28/28 + 静态检查 PASS。**板端 ≥2 次断电/重启复验 NOT_RUN**，上电后按 `deploy/REBOOT-RECOVERY-CHECKLIST.md` 取证 |
| 真实 ALSA/RT/health 检查 | **NOT_RUN** | 06 verify 的 V7/V8 在未接通时记 NOT_WIRED |
| MOTU 48k keeper（D8） | **BOARD-VERIFIED（task-154；证据 `rt-stack/evidence/motu-rate-rootcause/REPORT.md`）** | 板端 tpi-audio-dsp 已安装同一脚本/unit/timer/udev；静音 1s 48k，空闲 30min 保持 48k（见 `rt-stack/evidence/motu-rate-rootcause/REPORT.md`）；T71 仍须在目标板按本目录安装并由 V9 复验 |
| 传输层（scp/ssh） | 不在本目录 | 由 T71 owner 用既有 `deploy/ssh` 凭据完成；脚本本身不联网 |
| 真实 ALSA PCM 打开/duplex | **NOT_WIRED** | T155 host 只接线 ALSA owner 契约（`backend=fake`：rate/period/format/hwparams_hash 校验）；真实 open/读写/recover 仍属 T17-follow-up/T71 板端窗口 |
| 内核亲和/FIFO/IRQ 实写 | **WIRED（task-216/task-218；板端 NOT_RUN）** | host 启动期按 T45 计划真实写 `mlockall(MCL_CURRENT\|MCL_FUTURE)` + 主/I-O SCHED_FIFO(80) + worker SCHED_FIFO(70)+CPU 亲和，读回校验，失败 fail-closed（`crates/rt-host/src/rt_apply.rs`；`[rt] apply_affinity=true` 或 `TSPI_HOST_APPLY_AFFINITY=true`）。qemu Linux（sudo）实测 `kernel_applied=true`；非 root fail-closed rc=1。IRQ 线程优先级仍归板端 irq-affinity-pin（`irq_priority_applied=false`）；**板端真实生效待 T71 复跑** |
| 板端 systemd/真实设备/浏览器 | **NOT_RUN** | 未安装/未启动/未上板；daemon 控制面用 MemoryEngine 测试替身（真实 host engine 注入属 T26 follow-up，T70 evidence E-3） |
