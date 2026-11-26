# 部署（N150，Ubuntu x86_64）

设备上的 khaen 只允许三类落盘，**不在设备上编译**：

| 路径 | 内容 | 谁写 |
| --- | --- | --- |
| `/opt/khaen/{releases,current}` | 版本化发布；`current` 是指向 `releases/<version>` 的符号链接 | 只有 `install.sh` / `rollback.sh` |
| `/etc/khaen/` | 配置（`daemon.env` 等），由仓库模板渲染 | 部署脚本 + 人 |
| `/var/lib/khaen/` | 唯一状态目录（会话、校准产物索引） | daemon 自己 |

任何"手改设备上的二进制/手改 current"都视为违规：二进制必须能从某个提交重建（CI 产出 + manifest + sha256），切换只走脚本。

## 1. 发布产物

CI（`.github/workflows/release.yml`，打 `v*` 标签触发）产出：

    khaen-<version>-linux-x86_64.tar.gz
    khaen-<version>-linux-x86_64.tar.gz.sha256

包内布局：

    khaen-<version>-linux-x86_64/
      bin/khaen-daemon
      deploy/{install.sh,rollback.sh,khaen-daemon.service}
      SHA256SUMS
      manifest.json

`manifest.json`（schema `khaen-release/v1`）字段：

| 字段 | 含义 |
| --- | --- |
| schema | 固定 `khaen-release/v1` |
| version | 发布版本（来自标签） |
| commit | 构建该二进制的 git 提交（可追溯到源） |
| target | 目标三元组（当前只有 `x86_64-unknown-linux-gnu`） |
| files[] | 每个文件：`path`、`bytes`、`sha256` |

## 2. 安装

    scp khaen-<version>-linux-x86_64.tar.gz* n150:/var/tmp/
    ssh n150
    sudo /var/tmp/... # 解包后执行包内 deploy/install.sh
    sudo ./deploy/install.sh /var/tmp/khaen-<version>-linux-x86_64.tar.gz

脚本顺序：校验 tar.gz 的 sha256 → 解包到临时目录 → 逐文件校验 manifest → 安装到 `releases/<version>`（只读）→ **原子**切换 `current`（`ln -sfn` + `mv -Tf`）→ 安装 systemd unit → `systemctl restart`。任一步失败在切换前中止，设备上不留半成品。

`--no-restart` 只做准备、不重启（用于排练或错峰窗口）。

## 3. 回滚

    sudo ./deploy/rollback.sh              # 回到上一个版本
    sudo ./deploy/rollback.sh 0.1.2        # 回到指定版本

同样只做符号链接原子切换 + 重启，不动 `releases/` 里的任何文件。

## 4. 运行时

`khaen-daemon.service`：绑定 `127.0.0.1:8730`（`KHAEN_PORT` 可改），只服务本机；公网暴露由前置代理负责。配置走 `/etc/khaen/daemon.env`（`EnvironmentFile`，模板见 `deploy/daemon.env.example`），状态走 `/var/lib/khaen`。

### 4.1 设备监督器（T70c / T70e）

daemon 进程是设备生命周期状态机的**唯一 owner**（`Absent → Enumerated → ControlReady → RateAsserted → Streaming`）；采样率的事实来源是方案（`KHAEN_SAMPLE_RATE`），设备只是被断言 + 回读的对象。

| 环境变量 | 含义 | 不设置时 |
| --- | --- | --- |
| `KHAEN_DEVICE_ID` | 设备标识，在 `/sys/class/sound/card*/id` 里按子串匹配；先只读确认（`aplay -l` / `cat /proc/asound/cards`），N150 上 v1 审查记录的 id 是 `UltraLitemk5` | `device.wired=false` + 原因 |
| `KHAEN_RATE_READ` | 读速率命令（打印一个整数） | 同上（fail-closed，不猜速率） |
| `KHAEN_RATE_ASSERT` | 写速率命令（`{rate}` 占位符） | 同上 |
| `KHAEN_CONTROL_COMMAND` | 可选：拉起控制通道命令 | 走 ALSA 控制节点只读检查 |
| `KHAEN_DEVICE_POLL_MS` | 轮询间隔（默认 500） | 500 |

- 状态查询：`GET /api/device`（`state/plan_rate/device_rate/rate_asserted/attempts/last_error/safety`）；`POST /api/device/retry` 只清退避与锁存，**不会**自动解除安全静音。
- 安全联动 fail-closed：监督器锁存后 `safety.requires_mute=true`，daemon 每次轮询重新下发静音；用户在锁存期间解除静音也会在下一轮被压回去，恢复必须先把链路拉回 `Streaming`。
- 真实 MOTU 辅助脚本（读/写速率、拉起控制通道）属于装机产物，**当前仓库未提供，设备侧为 NOT_RUN**；本机测试覆盖的是监督器语义 + daemon 接线（假组件）。

### 4.2 控制面冒烟测试（只读 + 一次登录）

    KHAEN_SMOKE_PASSWORD='<管理员密码>' bash deploy/smoke-test.sh
    KHAEN_SMOKE_PASSWORD='<管理员密码>' bash deploy/smoke-test.sh http://127.0.0.1:8730 1
    # 也可用会话 token：KHAEN_SMOKE_TOKEN='<token>' bash deploy/smoke-test.sh

（Windows 检出把脚本记成 644，没有执行位；用 `bash` 调用即可。）

脚本先匿名 GET `/healthz` 与 `/api/auth/status`；认证未初始化时提示去 WebUI 走 setup 并以 0 退出。随后用 `KHAEN_SMOKE_PASSWORD` 登录（只建会话）或用 `KHAEN_SMOKE_TOKEN`，再带会话 GET `/api/meta` / `/api/status`（`khaen-status/v1`）/ `/api/device` / `/api/coeff`，校验结构与 API 版本。**不做任何设备写入、不 prepare/commit、不出声**；`device.wired=false` 或 rt-host 未连接属合法状态，只打印原因。

### 4.3 实时主机进程 `khaen-rt-host`（T70f）

与 daemon 分属两个进程：daemon 是控制面（REST + 设备监督器），`khaen-rt-host` 是数据面（AES67 收包 → 方案图 → ALSA PCM）。前者不打开音频流，后者不做 REST。

    sudo systemctl stop tspi-rt-host tspi-motu-48k-keeper 2>/dev/null || true   # 先让 v1/keeper 让出设备
    # card id 必须先只读确认（aplay -l / cat /proc/asound/cards）；N150 上 v1 记录是 UltraLitemk5，不是 MOTU。
    KHAEN_ALSA_DEVICE=hw:CARD=UltraLitemk5,DEV=0 \
    KHAEN_SAMPLE_RATE=48000 KHAEN_BLOCK_FRAMES=128 KHAEN_CHANNELS=2 \
    KHAEN_AES67_GROUP=239.69.67.1 KHAEN_AES67_PORT=5004 KHAEN_AES67_PAYLOAD_TYPE=97 \
    KHAEN_AES67_CHANNELS=2 KHAEN_AES67_MAP=0,1 KHAEN_JITTER_FRAMES=192 \
    KHAEN_PLAN=/var/lib/khaen/test.plan \
    KHAEN_RUN_SECONDS=1800 KHAEN_STATS_EVERY_BLOCKS=4000 \
    /opt/khaen/current/bin/khaen-rt-host

- **独占设备**：运行前确认没有 keeper / `prod_bridge.py` / aplay 占着 PCM；运行中不要从别的进程打开同一设备。
- **方案**：手工直启时启动读一次 `KHAEN_PLAN`（ABI v3，`prepare` 的准入与图编译在这里复用）。运行期换图**已实现**（T70f-b/T70f-c）：rt-host 默认 `KHAEN_CONTROL=on` 开控制 socket（`/run/khaen/rt-host.sock`），daemon 在 `KHAEN_RT_TARGET=rt-host`（默认）时经 `KHAEN_RT_SOCKET` 把 `prepare/commit/abort/mute` 真实下发到 rt-host；若手工直启时设了 `KHAEN_CONTROL=off`，就没有运行期换图入口，脚本不要假设。
- **未加载方案**时默认 `KHAEN_UNLOADED=silence`（fail-closed）；联调链路可用 `passthrough`。
- 设备监督器默认不装配（`SupervisorOptions::enabled=false`，状态如实 `absent`）；给齐 `KHAEN_DEVICE_ID`/`KHAEN_RATE_READ`/`KHAEN_RATE_ASSERT` 才装配真实监督器。
- 监控与 supervise 轮询目前在块线程上（每 `KHAEN_STATS_EVERY_BLOCKS` 块），**这是验证工具的让步，不是 RT 认证形态**；生产形态见 `docs/DESIGN.md` 的线程模型。
- 本仓库不提供 MOTU 读写速率的辅助脚本；在设备侧跑之前需要部署方提供（与 daemon 的 `daemon.env.example` 同一批产物），因此真机 30 分钟/8 小时验证仍为 **NOT_RUN**。
- **systemd 形态**：发布包内含 `deploy/khaen-rt-host.service` 与 `deploy/rt-host.env.example`；`install.sh` 只安装 unit、**不 enable、不启动**（数据面独占 ALSA 设备，出声需要窗口与许可），配置文件复制到 `/etc/khaen/rt-host.env` 且不覆盖已有文件。
- unit 里用 `Conflicts=tspi-rt-host.service tspi-motu-48k-keeper.service` 保证与 v1/keeper 互斥：设备被占时宁可起不来，也不和旧链路抢设备。
- **实时调度尚未认证**：unit 目前只给 `Nice=-10`，没有 `SCHED_FIFO`/RLIMIT_RTPRIO；真机 RT 调度与 xrun 长稳属于后续实测项，标记 **NOT_RUN**。
- **静音**：`Runtime::request_mute(bool)` 是块边界生效的用户静音；它与安全静音相互独立——用户解除静音不会清除安全故障，锁存期间 `run_block` 仍整块清零（fail-closed）。跨进程静音**已实现**（T70f-b）：daemon 的 `/api/mute` 在 `target=rt-host` 时经控制 socket 下发 `mute` op；`target=process` 只作用于 daemon 进程内引擎（离线测试），不会伪装成已作用于 rt-host。
- **无人值守取证**：设置 `KHAEN_STATUS_FILE=/var/lib/khaen/rt-status.json` 后每统计周期原子写一行 JSON（schema `khaen-rt-status/v1`：blocks/state/muted/safe/plan_rate/device_rate/rate_asserted/input/output/receiver/graph_id/last_error），soak 结束直接拷文件即可，不需要人盯控制台。

### 4.4 v1.2 运行时开关（daemon / rt-host）

| 环境变量 | 进程 | 含义 | 不设置/默认 |
| --- | --- | --- | --- |
| `KHAEN_RT_TARGET` | daemon | 控制 op 目标：`rt-host`（默认，经 UNIX socket）或 `process`（仅离线测试） | `rt-host` |
| `KHAEN_RT_SOCKET` | daemon | rt-host 控制 socket 路径，必须与 rt-host 的 `KHAEN_CONTROL_SOCKET` 一致 | `/run/khaen/rt-host.sock` |
| `KHAEN_RT_TIMEOUT_MS` | daemon | 单次往返超时（毫秒），必须大于 5 s 块边界 ack 上限 | `10000` |
| `KHAEN_CAL_BACKEND` | daemon | 校准 job 后端开关；`off/0` 关闭后如实 `not_wired` | 开 |
| `KHAEN_ALLOW_RESTART` | daemon | 允许 `/api/system/restart` 经 systemctl 重启 rt-host | 关（403） |
| `KHAEN_SPEAKER_TEST` | daemon | 通道识别测试音开关；`on` 才允许 `/api/speaker-test` | 关 |
| `KHAEN_SOUND_WINDOW` | daemon | 测试音出声窗口（`HH:MM-HH:MM` 逗号分隔，本地时间） | 不限（仍受开关约束） |
| `KHAEN_CAL` | rt-host | 进程内校准播放/采集开关；`off/0` 关闭 | 开 |
| `KHAEN_CAL_DIR` | rt-host | 校准产物目录 | `/var/lib/khaen/cal` |
| `KHAEN_UMIK_DEVICE/SERIAL/CAL_FILE/CAL_SHA256` | rt-host | UMIK 采集声明（必填项缺失即 `mic_declaration_missing`，不猜） | 未声明 |
| `KHAEN_CAL_CAPTURE_CHANNELS` | rt-host | 采集读块通道数（1ch 设备设 1） | `2` |
| `KHAEN_CAL_CHANNELS` / `KHAEN_CAL_LAYOUT` / `KHAEN_CAL_LEVEL_DBFS` | rt-host | 播放通道/布局/驱动电平 | `0,1,2,3,4,5,7` / `4.1.2` / `-12` |

- **MOTU/CueMix 写入边界（硬规则）**：只允许 4 类用途——切采样率、切光口模式（ADAT↔Toslink）、整体静音、整体音量；不做 per-channel 写入，不给未使用 DAC 静音。
- **测试音安全**：默认关；开启后仍要求 `audible:true`+`confirm:true`、单次 ≤10 s（默认 1 s、-30 dBFS）、自动恢复节目输出、`/api/mute` 可立即打断；真机出声验证需白天窗口与用户许可。
- **深度诊断**：`GET /api/status` 加法 `mode`（rt-host socket 主路径有真值；file/process/不可用 → null+reason）；`GET /api/meta` 加法 `speaker_test` 能力三态。

## 5. 验证状态（如实标注）

| 项 | 状态 |
| --- | --- |
| `cargo build --release -p khaen-daemon` 本机构建 | 实测通过（Windows x86_64） |
| CI release workflow 首次运行 | **NOT_RUN**（需要打 tag 后观察） |
| `install.sh` / `rollback.sh` 在 N150 上演练 | **NOT_RUN**（需要部署窗口；N150 当前仍跑 v1 的 `tspi-rt-host`） |
| 设备上原子切换 + 服务重启 | **NOT_RUN** |
| `khaen-rt-host` Windows 上编译（stub 路径） | 实测通过（`cargo check`，Linux 专属代码不参与） |
| `khaen-rt-host` Linux 编译 | 由 CI 覆盖（本机无 libasound/pkg-config，交叉检查跑不通） |
| `khaen-rt-host` 真机出声（30 min / 8 h） | **NOT_RUN**（需要设备窗口 + 你的许可，且需要部署方提供 MOTU 速率辅助脚本） |
| release 打包 `khaen-rt-host` + 两个 unit + 环境变量模板 | 本次已改 `release.yml`/打包清单；**NOT_RUN**（需下次打 tag 实测） |

| 五个校准命令行工具离线测试（Windows） | 实测通过（`cargo test -p khaen-cal-tools`，含 WAV/判定/运行包） |
| 发布包打包校准工具 + 闭环文档 + 包装脚本 | **NOT_RUN**（本次已改打包清单，需下次打 tag 实测） |
| 真机闭环复测（基线录音 → 下发 → 施加录音 → 判定） | **NOT_RUN**（需要 MOTU ALSA PCM 名 + 出声窗口许可） |

## 6. 校准命令行工具包与闭环复测入口

发布包（`linux-x86_64`）现在除 `khaen-daemon`、`khaen-rt-host` 外，还包含五个离线校准工具：

| 二进制 | 作用 |
| --- | --- |
| `khaen-single-point` | 合成场景单点求解 → 参数/报告/方案/清单 |
| `khaen-plan-levels` | 方案文件 → 预测电平 |
| `khaen-band-levels` | 两段 WAV → 实测相对电平 |
| `khaen-closed-loop` | 两份电平集合 → 三态报告 |
| `khaen-closed-loop-run` | 方案 + 两段 WAV → 四文件证据包 |

包内同时带 `docs/calibration-cli.md`（命令行用法）与 `docs/CLOSED-LOOP.md`（闭环协议与现场手册），以及 `deploy/closed-loop-verdict.sh` 设备侧包装脚本。

现场流程与判据见 `docs/CLOSED-LOOP.md`：只读枚举 → 30 分钟基线 → 8 小时浸泡 → 基线录音 → 下发方案 → 施加录音 → `khaen-closed-loop-run` 判定 → 不通过回滚。

验证状态（如实）：离线工具与判定链路在 Windows 上测试通过；发布包打包与设备上执行 **NOT_RUN**（需要打 tag 与出声窗口）。
