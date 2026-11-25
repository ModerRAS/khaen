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
| `KHAEN_DEVICE_ID` | 设备标识，在 `/sys/class/sound/card*/id` 里按子串匹配 | `device.wired=false` + 原因 |
| `KHAEN_RATE_READ` | 读速率命令（打印一个整数） | 同上（fail-closed，不猜速率） |
| `KHAEN_RATE_ASSERT` | 写速率命令（`{rate}` 占位符） | 同上 |
| `KHAEN_CONTROL_COMMAND` | 可选：拉起控制通道命令 | 走 ALSA 控制节点只读检查 |
| `KHAEN_DEVICE_POLL_MS` | 轮询间隔（默认 500） | 500 |

- 状态查询：`GET /api/device`（`state/plan_rate/device_rate/rate_asserted/attempts/last_error/safety`）；`POST /api/device/retry` 只清退避与锁存，**不会**自动解除安全静音。
- 安全联动 fail-closed：监督器锁存后 `safety.requires_mute=true`，daemon 每次轮询重新下发静音；用户在锁存期间解除静音也会在下一轮被压回去，恢复必须先把链路拉回 `Streaming`。
- 真实 MOTU 辅助脚本（读/写速率、拉起控制通道）属于装机产物，**当前仓库未提供，设备侧为 NOT_RUN**；本机测试覆盖的是监督器语义 + daemon 接线（假组件）。

### 4.2 控制面冒烟测试（只读）

    bash deploy/smoke-test.sh                     # 默认 http://127.0.0.1:8730
    bash deploy/smoke-test.sh http://127.0.0.1:8730 1

（Windows 检出把脚本记成 644，没有执行位；用 `bash` 调用即可。）

检查 `/api/status` / `/api/device` / `/api/coeff` 的结构与协议版本，**不做任何设备写入、不 prepare/commit、不出声**。`device.wired=false` 属合法状态（未配置监督器），只打印原因；设备 `faulted` 同样只打印，是否升级处理由人工判断。

### 4.3 实时主机进程 `khaen-rt-host`（T70f）

与 daemon 分属两个进程：daemon 是控制面（REST + 设备监督器），`khaen-rt-host` 是数据面（AES67 收包 → 方案图 → ALSA PCM）。前者不打开音频流，后者不做 REST。

    sudo systemctl stop tspi-rt-host tspi-motu-48k-keeper 2>/dev/null || true   # 先让 v1/keeper 让出设备
    KHAEN_ALSA_DEVICE=hw:MOTU,0 \
    KHAEN_SAMPLE_RATE=48000 KHAEN_BLOCK_FRAMES=128 KHAEN_CHANNELS=2 \
    KHAEN_AES67_GROUP=239.69.67.1 KHAEN_AES67_PORT=5004 KHAEN_AES67_PAYLOAD_TYPE=97 \
    KHAEN_AES67_CHANNELS=2 KHAEN_AES67_MAP=0,1 KHAEN_JITTER_FRAMES=192 \
    KHAEN_PLAN=/var/lib/khaen/test.plan \
    KHAEN_RUN_SECONDS=1800 KHAEN_STATS_EVERY_BLOCKS=4000 \
    /opt/khaen/current/bin/khaen-rt-host

- **独占设备**：运行前确认没有 keeper / `prod_bridge.py` / aplay 占着 PCM；运行中不要从别的进程打开同一设备。
- **方案**：启动时读一次 `KHAEN_PLAN`（ABI v3，`prepare` 的准入与图编译在这里复用）；运行期不换图——跨进程换图通道（T70f 后续）**未实现**，脚本里不要把 daemon 的 commit 当成已作用于 rt-host。
- **未加载方案**时默认 `KHAEN_UNLOADED=silence`（fail-closed）；联调链路可用 `passthrough`。
- 设备监督器默认不装配（`SupervisorOptions::enabled=false`，状态如实 `absent`）；给齐 `KHAEN_DEVICE_ID`/`KHAEN_RATE_READ`/`KHAEN_RATE_ASSERT` 才装配真实监督器。
- 监控与 supervise 轮询目前在块线程上（每 `KHAEN_STATS_EVERY_BLOCKS` 块），**这是验证工具的让步，不是 RT 认证形态**；生产形态见 `docs/DESIGN.md` 的线程模型。
- 本仓库不提供 MOTU 读写速率的辅助脚本；在设备侧跑之前需要部署方提供（与 daemon 的 `daemon.env.example` 同一批产物），因此真机 30 分钟/8 小时验证仍为 **NOT_RUN**。

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
