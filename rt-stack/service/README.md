# T28/T155 service —— 新栈 systemd 单元（T71 安装；本目录不等于已上板）

> WBS T28（release-owner）。**状态（2026-11-17 修复后）：单元与 CLI 契约已冻结（T155），仍未安装/未上板。**
> - `khaen-rt-host`：aarch64 gnu 已 BUILT（768,384 B，sha256 e011745c…，见 `evidence/final-wiring/`）；`run --config` WIRED
>   （真实 ALSA 播放/duplex 仍 NOT_WIRED，backend=fake）。
> - `tspi-rt-daemon`：aarch64 gnu 已 BUILT（1,210,296 B，sha256 009a110e…）；`serve --config` WIRED；真实板端运行 NOT_RUN。
> - 本目录是 T71 的**安装源**：`04-install-release.sh` 从 release bundle 的 `units/` 复制；改动后必须重新打 bundle
>   （T155 冻结 bundle 早于 F-17/F-18 修复，含旧 `TSPI_RT_HOST_SAFE_MODE` 与无 Wants 的 target，不得直接上板，见 deploy/README §6.1）。
> - **2026-11-18 板窗缺陷修复（F3/F4；证据 `rt-stack/evidence/cursor-fix-deploy-f1f5/`）**：
>   ① 两 unit 增 `RuntimeDirectory=tspi-rt-stack`/`LogsDirectory=tspi-rt-stack`（`ProtectSystem=strict` 下
>   `ReadWritePaths` 要求 /run、/var/log 目录在 mount namespace 建立前存在，否则 host/daemon 启动即崩）；
>   ② host `DeviceAllow=/dev/snd/* rw` → `DeviceAllow=char-alsa rw`（systemd 252 + cgroup v2 下设备通配不生效，
>   板端实测所有 ALSA 节点 EPERM；char-alsa = ALSA 设备组，保持只放行 ALSA 的原意）。修复后须用**重打的
>   release-v4 bundle**（`m4-15cf411-20261118-v4`）上板，见 deploy/README §6.2。

> - **2026-11-20 B1 开机 IRQ 竞态修复（task-262）**：`tspi-irq-affinity.service` 增
>   `ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait` + `TimeoutStartSec=120`（v11 重启实测 oneshot 早于
>   end0 IRQ56/57 节点就绪 → apply fail-closed → host 因 Requires= 不启动）；`04-install-release.sh` 安装时迁移板端
>   legacy live-only drop-in `10-wait-irq.conf`；证据 `rt-stack/evidence/irq-boot-persist/`。
## 1. 文件

| 文件 | 作用 | 状态 |
| --- | --- | --- |
| `tspi-rt-stack.target` | 新栈总目标：**Wants/After host+daemon**（F-18/T28-P0-1；只 start target 即可拉起两者） | 冻结（T155 + F-18 修复） |
| `khaen-rt-host.service` | 自研 RT DSP host：唯一 ALSA/设备 owner；RT 门槛 LimitRTPRIO=99/LimitMEMLOCK=infinity；**F3** RuntimeDirectory/LogsDirectory=tspi-rt-stack；**F4** DeviceAllow=char-alsa rw | 冻结（CLI WIRED；真实 ALSA NOT_WIRED；2026-11-18 F3/F4） |
| `tspi-rt-daemon.service` | 控制面/校准 daemon：REST/WS/Web + calibration job 唯一控制面；不直接打开 PCM；**F3** RuntimeDirectory/LogsDirectory=tspi-rt-stack | 冻结（bin BUILT/WIRED；板端 NOT_RUN；2026-11-18 F3） |
| `tspi-irq-affinity.service` | **task-192 + B1**：T45 依赖 IRQ 亲和持久化 oneshot（`Before=khaen-rt-host.service`，`RemainAfterExit=yes`，ExecStart=`/usr/local/lib/tspi/irq-affinity-pin apply`）；**B1**：`ExecStartPre=…irq-affinity-pin wait` 先等 IRQ 节点就绪 + `TimeoutStartSec=120`；host 启动前把 IRQ58/56/57 钉回 A53 并回读校验 | 新增（B1 固化：本机 mock 28/28；板端重启 NOT_RUN） |
| `khaen-rt-host.service.d/90-safe-mute.conf.example` | 回滚兜底 drop-in：追加 `EnvironmentFile=/etc/tspi-rt-stack/90-safe-mute.env`（保护静音，绝不回 C） | 冻结（F-17 修复） |
| `khaen-rt-host.service.d/90-safe-mute.env.example` | **host 实际读取的** `TSPI_HOST_SAFE_MODE=mute`（F-17；不得写 `TSPI_RT_HOST_SAFE_MODE`） | 冻结（F-17 修复） |

配套配置模板在 `deploy/config/{rt-host,rt-daemon}.{env,toml}.example`：`04-install-release.sh` 首次安装到
`/etc/tspi-rt-stack/`（不覆盖既有）。**缺 toml 时 host/daemon 启动即退**，01-preflight 与 06-verify 都把配置存在性当硬门（T28-P0-2）。

## 2. 设计要点

- **单设备 owner**：只有 host 打开 ALSA/MOTU；daemon 通过 host 控制面工作，不打开 PCM（设备独占由 `deploy/bin/02-check-device-exclusive.sh` 验证）。
- **防误启守卫**：host/daemon 均有
  `ConditionPathExists=!/run/tspi/camilladsp.yml` 与 `ConditionPathExists=!/etc/tspi/last-mode`；
  旧 C 激活入口未清理时 unit 拒绝启动，避免未退役板子上双音频 owner。
- **无 C 依赖**：单元 After/Wants/PartOf 只引用自身 target 与 network-online；`deploy/tests/check_unit_syntax.py` 拒绝任何旧 C 依赖，
  并静态校验 target 的 Wants/After、IRQ oneshot 契约与 safe-mute 变量名（F-17/F-18/task-192 回归）。
- **IRQ 亲和持久化（task-192）**：T45 admission 要求 USB/PTP/eth 依赖 IRQ 不落在 DSP_CPUS={4,5,6,7}；
  task-189 实测 irqbalance 会把已迁 A53 的 IRQ58/56/57 在 ≤25s 内搬回 A72。因此 host `Requires=`/`After=`
  `tspi-irq-affinity.service`：oneshot 失败 → host 不启动（fail-safe）；`04-install-release.sh` 同时
  disable+stop irqbalance 并写 pin，preflight J / verify V10 双重校验。见 deploy/README §5.1。
- **B1 开机竞态等待（2026-11-20 板窗）**：`tspi-irq-affinity.service` 增
  `ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait` + `TimeoutStartSec=120`——v11 重启实测 oneshot 早于
  end0 IRQ56/57 节点就绪，apply fail-closed → host 因 `Requires=` 不启动；现在先等节点
  （`TSPI_IRQ_WAIT_SECS`，默认 60s）再钉核，超时仍 fail-closed。04 安装时迁移 legacy live-only drop-in；
  01 J / 06 V10 增 bootwait/legacy-dropin 回归门。
- **RT 门槛**：host `LimitRTPRIO=99`、`LimitMEMLOCK=infinity`、`Nice=-10`、CAP_SYS_NICE/CAP_IPC_LOCK；
  T18 admission 的 mlockall/预触页失败必须拒启主模式，由 06-verify 检查 /proc/<pid>/limits（--require-rt 硬门）。
- **保留 AoIP/PTP**：`tspi-statime-inferno` 等 5 个旧 unit 属 P1 保留（compat-matrix A11），T71 不 retire，
  但切换窗口内必须 inactive（audio-off）；后续由新 control 接管生命周期（T44 follow-up）。
- **安全静音（F-17）**：systemd.exec(5) 中 `EnvironmentFile=` 的变量**覆盖** `Environment=`；主 unit 的
  `rt-host.env` 已写 `TSPI_HOST_SAFE_MODE=normal`，所以 drop-in 不能靠 `Environment=` 覆盖，而是追加一个
  排在主 unit 之后的 `EnvironmentFile=90-safe-mute.env`。`08-rollback.sh --mute` 同时安装两者，并核对
  `TSPI_HOST_SAFE_MODE=mute` 已生效：**F5（2026-11-18）**改为读 `/proc/<MainPID>/environ`（进程真实环境），
  回退解析 `systemctl show -p EnvironmentFiles`；**不再用** `systemctl show -p Environment`——该属性不含
  `EnvironmentFile` 内容，板端实测会误报 `NOT_APPLIED`（假阴性）。

## 3. 安装/启用/停止顺序（由 deploy 脚本执行，不在本目录手动执行）

1. `05-atomic-switch.sh` 先在 **install 之前**跑 `01-preflight-switch.sh --pre-install`（只读；失败=完全零变更）。
2. `04-install-release.sh --live --no-enable`：复制 units 到 `/etc/systemd/system/`，安装/保留 `/etc/tspi-rt-stack/{env,toml}`，
   安装 `irq-affinity-pin`/`irq-affinity.env`，**disable+stop irqbalance 并写 IRQ pin**，`daemon-reload`，**不 start**；
   keeper unit/udev 失败硬失败（F-43），bundle 缺 `tspi-irq-affinity.service` 或该 unit 缺 B1 wait 前置也硬失败；
   安装时迁移/删除板端 legacy live-only drop-in（`tspi-irq-affinity.service.d/10-wait-irq.conf` → evidence）。
   keeper unit/udev 失败硬失败（F-43），bundle 缺 `tspi-irq-affinity.service` 也硬失败（task-192）。
3. `01-preflight-switch.sh`（完整 A–J）：校验 release/manifest/arch、unit、**toml 配置**、192k/176.4k 禁令、keeper/udev、
   **J IRQ 持久化**（irqbalance 未 enabled/active、oneshot+env 已安装、IRQ effective 已在 A53 且 T45 IRQ 前置 PASS）。
4. `03-retire-c.sh`：旧 C/shim/入口 stop+disable(+mask)，清除 /run/tspi 旧 marker。
5. `05` 在 retire 之后才 start keeper → **start `tspi-irq-affinity.service`（钉核；ExecStartPre 先等 IRQ 节点就绪）** →
   `enable/start tspi-rt-stack.target`，并**逐个等 `khaen-rt-host.service`、`tspi-rt-daemon.service` active**
   （F-18/T28-P0-a；超时即回滚 last-good/mute）。
6. 停止：`systemctl stop tspi-rt-stack.target`（PartOf 传播）；不得用旧 tspi-mode/camilladsp 入口。
7. 回退：`08-rollback.sh` 只回自研 last-good 或 mute；C 保持 masked。

## 4. 未测项（NOT_RUN，禁止当已验证）

- 单元未安装到任何板子；真实 `systemd-analyze verify`/systemd 启动/restart/stop 未执行（本机无 systemd，只有静态检查 + mock dry-run）。
- task-192 IRQ 持久化**板端 NOT_RUN**：irqbalance disable/stop、`/proc/irq/*/smp_affinity_list` 写核、oneshot 与 host
  的启动顺序仅在本机 mock 夹具验证（现行 dry-run 28/28）；板端须在 T71 窗口内实跑并由 preflight J / verify V10 取证。
- **B1 开机 IRQ 竞态（2026-11-20 板窗）的修复只在 unit/脚本/mock 层验证**：`ExecStartPre=… wait` 的时序
  （开机 IRQ56/57 节点晚到 → 等待后钉核）与 legacy drop-in 迁移**未在真实 systemd 上执行**；板端须断电/重启
  ≥2 次复验（V10.bootwait/V10.legacy-dropin PASS、19/19），见 `deploy/REBOOT-RECOVERY-CHECKLIST.md`。
- **2026-11-18 F3/F4 的修复只在 unit 文件与 mock 夹具层验证**（F4 的 `char-alsa` 设备组语义、RuntimeDirectory
  的 namespace 建立顺序需板端 systemd 252 实测）；本机无 systemd，不能替代板端 `systemd-analyze verify`/启动验证。
- 真实 ALSA PCM open/duplex、真实 RT 亲和/FIFO/IRQ 未接线（host backend=fake）。
- T71 板端切换/回滚、T72 无 C 运行证明 NOT_RUN。
- T155 冻结 bundle 与修复后的 service/ 已不同：上板必须用 **release-v4（`m4-15cf411-20261118-v4`）** 的
  bundle/上传包（2026-11-18 F1–F5 重打；见 `rt-stack/evidence/release-v4/` 与 deploy/README §6.2）。
