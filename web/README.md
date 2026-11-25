# tspi-audio-web · 泰山派3M 音频 DSP 设备 Web 管理服务

面向泰山派3M（RK3576）+ Debian 12 + PREEMPT_RT 音频 DSP 设备的局域网 Web 管理服务：
Rust + axum 单二进制，内置中文前端（无构建步骤、无外网 CDN），开机自启，通过 systemd /
CamillaDSP websocket / linuxptp / journalctl / ALSA 管理整台设备的音频链路。

一期范围：认证、总览、CamillaDSP、PTP、AES67 / Inferno、ALSA/MOTU、CueMix 矩阵、运行模式切换
（dante / aes67 / off）、音频格式有限编辑、reboot/shutdown、TSPI_MOCK 全 mock。

## 目录

| 路径 | 说明 |
| --- | --- |
| `src/` | Rust 源码（config/auth/routes/backends/mode 控制器/内嵌资源） |
| `assets/` | 前端静态文件与测试音（编译期内嵌进二进制） |
| `tests/api.rs` | 路由/集成测试（tower `ServiceExt`，无需监听端口） |
| `deploy/tspi-audio-web.service` | systemd unit |
| `deploy/config.toml.example` | 配置模板（含全部注释；首启自动生成配置也用这份） |
| `docs/API.md` | REST API 文档（与实现同步维护） |
| `docs/MOTU-CUEMIX.md` | UltraLite-mk5 CueMix 协议与页面对应 |
| `scripts/cross-build.sh` | aarch64-unknown-linux-gnu 交叉编译脚本 |
| `scripts/install-device.sh` | 设备安装 / OEM rootfs 镜像预装脚本（支持 `--dry-run`） |
| `deploy/OEM-HANDOFF.md` | OEM 预装交接说明（布局/权限/首启 token/验收命令） |
| `dist/` | aarch64 构建产物：`tspi-audio-web`、`SHA256SUMS`、`INSTALL-MANIFEST.md`、构建/验证日志 |

## 快速开始（Windows 本地 mock，无需设备）

前置：Rust 1.80+（本工程用 1.99.1 验证；`cargo/rustc/rustup` 已装即可）。

~~~powershell
cd projects\\tspi-audio-dsp\\web
$env:TSPI_MOCK = "1"
cargo run
# 浏览器打开 http://127.0.0.1/（默认端口 80，无需记端口）
# 若本机 80 被占用或非 root（Linux/macOS 不能绑 <1024）：cargo run -- --listen 127.0.0.1:8088
# mock 模式默认 token：mock-token
~~~

mock 模式（TSPI_MOCK=1 或 `--mock`）全部后端走内存：

- 不会执行任何真实 `systemctl` / `journalctl` / `pmc` / `aplay` / `ip` 命令（命令执行器一律拒绝）；
- 不读写真实 /etc、/proc；配置/服务/模式状态都在内存模拟；
- 用于本机开发、UI 验证、自动化测试与演示失败回滚。

常用 mock 演示开关（详见 API.md `12）：

~~~powershell
$env:TSPI_MOCK_STEP_DELAY_MS = "150"          # 切换每步延迟，方便看进度
$env:TSPI_MOCK_FAIL_START = "inferno.service"  # 演示 Dante 切换失败 → 自动回滚
$env:TSPI_MOCK_FAIL_APPLY = "1"                # 演示音频格式修改失败回滚
$env:TSPI_MOCK_PERSISTED_MODE = "dante"        # 预置上次模式（仅展示，不自动恢复）
$env:TSPI_MOCK_CHANNELMAP_PRESET = "5.3.2"     # 预置声道分配文档（跳过首启向导）
$env:TSPI_MOCK_CHANNELMAP_FAIL_APPLY = "1"     # 演示 channelmap apply 失败 → 显示回滚提示
cargo run
~~~

## 配置

### 配置文件位置

| 平台 | 默认路径 |
| --- | --- |
| Linux（设备） | `/etc/tspi-audio-web/config.toml`（0600，root:root） |
| Windows（开发） | `.\\config.toml`（当前目录；可不存在，mock 下用内置默认值） |

优先级：`--config <路径>` > 环境变量 `TSPI_CONFIG` > 默认路径。

### 首次启动、管理密码与 token

- **首次打开 Web**（`auth_file` 不存在）：显示“首次设置管理密码”页；设置至少 8 位（推荐 10+）密码后立即登录，
  Argon2id 哈希写 `/etc/tspi-audio-web/auth.json`（0600）。已设置后不再允许 setup，直接显示登录页。
- **密码登录**：登录页输入管理密码；会话 Cookie/CSRF 与旧流程一致。
- **token 保留为恢复/CLI 兜底**：设置密码后 token 仍有效（登录页展开“忘记密码/使用 token”，或 `X-Auth-Token` / Bearer），
  不会因设置密码失效；适合排障/自动化。首次设置前也可继续用 token 登录（旧配置迁移不锁死）。
- 配置文件不存在（非 mock）：自动按 `deploy/config.toml.example` 模板生成，随机生成 32 位十六进制 token，
  写入文件（0600）并**打印到 stdout/journald**（"已生成新的访问 token" 横幅）。
- 查看 token：`sudo tspi-audio-web --config /etc/tspi-audio-web/config.toml --print-token`。
- 限流/锁定：同一客户端密码/token 连续失败 5 次锁定 `login_lockout_secs`（默认 60s）。
- 首设无认证是局域网便利与安全的折中：请仅在可信网络完成首次设置；暴露公网前务必先设置密码/加 HTTPS 反代。

### 关键配置项

完整项见 `deploy/config.toml.example`；常用：

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `listen` | `0.0.0.0:80` | 监听地址（默认 80：局域网直接 `http://<设备IP>/`，无需记端口；systemd 以 root 运行可绑定；8080 留给 AES67 daemon；如 80 被占用改 `0.0.0.0:8088` 回退） |
| `token` | `CHANGE_ME`（首启轮换） | 恢复/CLI 兜底 token（32 hex）；启动时同步到 `TSPI_WEB_TOKEN_FILE`（默认 `/etc/tspi/web-token`） |
| `auth_file` | `/etc/tspi-audio-web/auth.json` | Argon2id 管理密码哈希（0600）；不存在=首设密码页 |
| `session_ttl_secs` | 43200 | 浏览器会话有效期 |
| `cookie_secure` | false | 经 HTTPS 反代时设 true |
| `[camilladsp] ws_url` | `ws://127.0.0.1:1234` | CamillaDSP websocket 地址 |
| `[camilladsp] service / config_file / config_dir` | tspi-camilladsp.service；/etc/camilladsp/… | 服务名与配置文件（OEM v2 单元名） |
| `[ptp] conf_file / pmc_bin` | `/etc/ptp4l.conf` / `pmc` | linuxptp 配置与 pmc |
| `[ptp] ptp4l/phc2sys/statime_service` | tspi-ptp4l-master / tspi-phc2sys / tspi-statime-inferno | OEM v2 实际单元名 |
| `[aes67] service / config_files` | tspi-aes67-daemon.service；… | AES67 daemon 服务与只读配置文件 |
| `[inferno] service / config_files` | tspi-statime-inferno.service；/etc/default/inferno 等 | Inferno 本身是 ALSA 插件、无独立服务；此单元仅作 PTPv1 状态/日志 |
| `[alsa] aplay_bin / default_device` | aplay；空（自动优先 MOTU） | 测试音 |
| `[system] allow_reboot / allow_shutdown` | false / false | 电源操作开关（开启后仍要输入确认短语） |
| `[mode] switch_script` | `/usr/local/bin/tspi-mode` | audio-stack 统一切换脚本 |
| `[mode] restore_on_start` | **false** | D4：默认开机不自动恢复音频模式 |
| `[mode] enabled` | true | 是否允许 Web 端切换运行模式 |

> 服务状态页会自动发现板上实际存在的 `tspi-*.service`（tspi-camilladsp / tspi-ptp4l-master /
> tspi-phc2sys / tspi-statime-inferno / tspi-aes67-daemon / tspi-ravenna-lkm），不依赖配置里的旧通用名；
> Inferno 行会标注“ALSA 插件，无独立 systemd 服务”。

### 环境变量覆盖（优先于配置文件）

| 变量 | 作用 |
| --- | --- |
| `TSPI_CONFIG` | 配置文件路径（低于 `--config`） |
| `TSPI_MOCK=1` | 全内存 mock 模式 |
| `TSPI_LISTEN` | 监听地址，如 `0.0.0.0:80`（默认）或回退 `0.0.0.0:8088` |
| `TSPI_TOKEN` | 临时覆盖 token（不改文件） |
| `TSPI_AUTH_FILE` | 覆盖管理密码哈希文件路径（默认 `/etc/tspi-audio-web/auth.json`） |
| `TSPI_ALLOW_REBOOT=1` / `TSPI_ALLOW_SHUTDOWN=1` | 开启电源按钮 |
| `TSPI_MODE_SCRIPT` | 覆盖切换脚本路径 |
| `TSPI_STATE_FILE` | 覆盖模式持久化文件 |
| `TSPI_MODE_ENABLED` / `TSPI_RESTORE_MODE` / `TSPI_CAMILLADSP_WS` | 对应配置覆盖 |

命令行：`--config <路径>`、`--listen <地址>`、`--mock`、`--print-token`、`--help`、`--version`。

## 功能说明

### 总览
服务状态、CPU/内存/温度/uptime/网络 IP、内核与 RT 状态（`/sys/kernel/realtime` + `/proc/cmdline`）、
PTP 状态、CamillaDSP 状态/xrun 线索、声卡列表（`/proc/asound` + USB）。

### CamillaDSP
状态/版本/配置/音量/xrun（日志推断）、start/stop/restart、列出并切换 `/etc/camilladsp/configs` 下的配置
（服务端 serde_yaml 校验；优先 websocket `SetConfigJson`，失败回退 systemd restart）、配置只读预览。
协议实现依据 HEnquist/camilladsp 的 `websocket.md` 并核对 4.1.3 源码，**不编造命令**。

### PTP
- 当前模式对应的 PTP 来源/角色/锁定状态；
- linuxptp（方案 B2）：`pmc -u -b 0 "GET TIME_STATUS_NP"` / `"GET PORT_DATA_SET"`；
- AES67 方案 B1：PTP 由 aes67 daemon 内置，不启动 ptp4l，锁定状态按 daemon 日志或 `tspi-mode status` 的 `ptp_lock`；
- Dante：Statime PTPv1，按 statime 日志推断（pmc 不适用）；
- 配置：`/etc/ptp4l.conf` 的 `masterOnly` / `priority1` —— 先备份、只改 `[global]`、写后校验、重启 ptp4l/phc2sys，失败自动恢复备份。

### AES67 / Inferno
服务状态、journalctl 最近日志、start/stop/restart、配置只读展示。
Inferno 上游无统一配置文件（`INFERNO_*` 环境变量 / ALSA 插件参数），页面展示部署时指定的只读文件（可配置）。

### ALSA / MOTU
声卡列表、PCM 设备、活动 hw_params（`/proc/asound` 实时）、USB 设备（识别 MOTU `07fd`）、
测试音按钮：内置 1 kHz / 48 kHz / 16-bit / 1 秒 WAV，经 `aplay -D plughw:X,Y` 播放（设备名白名单，不经过 shell）。

### MOTU / CueMix

独立页（`data-page="motu"`），约 8 秒轮询 `GET /api/motu/status` 与 `/api/motu/snapshot`。
设备卡、Main 音量（`kMainTrim`，滑块 0–80，显示 −n dB，确认后写入）、直通列表、
「全部静音（会关闭 CueMix 输入直通）」与「恢复快照」。矩阵和非零交叉点只读。
协议、final insert 的 280 格候选、以及和 `tools/motu/cuemix_client.py` 的差异见 `docs/MOTU-CUEMIX.md`。

`TSPI_MOCK=1` 使用内置 UltraLite-mk5 状态（含 in7→out7、in8→out8、Main −28 dB），不发现网卡、不连 `ws://:1280`。
`TSPI_MOCK_MOTU_APPLY_FAILS=1` 让写入返回回滚。真机快照在 `[motu] snapshot_dir`，默认只留 20 份。

### 运行模式（dante / aes67 / off）

GET/PUT `/api/mode`；切换为异步任务，互斥（在途 409），失败自动回滚并持久化 last_error；
当前模式写 `/etc/tspi-audio-web/state.json`。

- 优先调用 audio-stack 的 `/usr/local/bin/tspi-mode <mode> --json`，再用 `tspi-mode status --json` 读取契约状态，
  最后由 Web 独立复核（CamillaDSP Running / 声卡 / PTP）。
- 脚本不存在时使用内置编排（组件/顺序来自 `[mode.dante]` / `[mode.aes67]` 配置）：
  `停 CamillaDSP → 停当前 AoIP → 停当前 PTP → 启目标 PTP（若有）→ 等锁 → 启目标 AoIP → 等就绪 → 应用 dante.yml/aes67.yml → 启 CamillaDSP → 校验`。
- **AES67 默认方案 B1**：本机不做 PTP master，aes67 daemon 自带 PTPv2 slave，`[mode.aes67].ptp_services = []`，
  切换不启动 ptp4l/phc2sys；只有改成 B2（含 ptp4l/phc2sys）才走 linuxptp GM 路径。
- **D4 口径**：默认 `restore_on_start = false` —— 开机音频 off，Web 启动不自动调用 tspi-mode；页面显示
  off + “上次持久化模式（未加载）”。需要开机恢复时显式设 `restore_on_start = true`。
- 模式卡片显示：当前模式/切换进度/步骤、PTP 版本+来源+角色+锁定、AoIP 服务、CamillaDSP 输入/输出设备、
  采样率/位深/声道/chunksize/target_level、xrun 线索。

tspi-mode 退出码：0 就绪 / 2 参数 / 3 前置 / 4 设备 / 5 PTP 未锁 / 7 DSP 启动失败 / 8 自检失败 / 9 已回滚 / 10 另一切换进行中。
状态 JSON：schema=`tspi-audio/v1`，顶层含 `ptp_role`/`ptp_lock`/`pid`，嵌套 `ptp`/`aoip`/`camilladsp`/`alsa`/`last_error`。

### 音频格式与缓冲（有限编辑）
页面显示当前模式对应的采样率、位深、声道、chunksize、target_level、xrun；可改**采样率 / chunksize / target_level**
（范围校验；32 帧允许但显示高风险提示，默认建议 128）。改动：读当前 CamillaDSP YAML → 改 `devices` 字段 →
服务端 serde_yaml 校验 → 备份 → 原子写 + websocket 重载（失败回退 systemd restart，再失败自动回滚并报错）。

### 声道分配 / 校准（channelmap v2）

第 9 页「声道分配 / 校准」直接对接 audio-stack 的真实契约：

- active：`/etc/tspi-audio/channelmap.yml`（schema `tspi-channelmap/v2`，首启无文件属正常）；
- 预设：`/etc/tspi-audio/layout-presets.yml`（schema `tspi-layout-presets/v2`，随固件只读；WebUI 只读；
  含 RZ50 布局集 4.0/4.1/4.1.2/5.1.4/7.1.2/7.2 等，4.1.2 为独立布局；remap_presets 仅同布局
  5.1.2->5.1.2，**不做跨布局 remap/upmix**）。Web 内嵌副本与板端部署文件**逐项一致**
  （sha256 `48561092…`）；**4.1.2 = FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7（ch6 空）**，即 MOTU
  Line Out 1-6/8，SUB=Line Out 8；`tests/presets_drift.rs` 在 cargo test 里比较两侧 role→outputs /
  输入映射 / remap preset，任何漂移直接失败（详情见该测试；`TSPI_DEPLOY_LAYOUT_PRESETS` 可覆盖部署文件路径））；
- profiles：`/etc/tspi-audio/profiles/*.yml`；FIR：`/etc/tspi-audio/coeffs/`；
- 工具：板端 `/usr/local/bin/tspi-channelmap`（Python）是**校验/渲染/apply 的唯一权威**，Web 只委托 + 展示
  （probe/validate/print/render/apply/rollback/profiles/save-profile/load-profile/export/import），退出码 0/2/3/4/5/9；
  apply 的 9 = 失败已回滚 last-good，UI 直接显示。CLI 缺失/不可用时 validate/render 返回 503
  （"tspi-channelmap 不可用，无法校验/渲染"），**不再用本地规则放行**；mock 模式才能用与板端对齐的本地规则（测试/演示）。

页面能力：

- **布局**：输入布局（H16 5.1/7.1/5.1.2/5.1.4/7.1.2/7.1.4，streams 每流 ≤8ch；7.1.4=2 流）与输出布局（2.0/2.1/5.1/5.1.2/5.2/5.3.2/7.1.4/9.1.4）从预设读取；
  输入/输出通道数支持 auto 或固定值；tspi-channelmap probe 成功则显示 playback/capture 的 min~max 通道、格式、采样率，失败时让用户手填。
- **声道域（不要混用）**：角色 id ≠ 布局槽位 ≠ 设备通道。`output.roles[].outputs` 是 **0-based 布局槽位**（API/UI 展示的都是槽位，
  不是 MOTU 设备通道号）；MOTU 设备恒 **22ch**（`output.channels` 只是无 probe 时的 fallback）。4.1.2 用槽位 0-5,7（8 槽，ch6 空；
  HFL=4、HFR=5、SUB1=7=Line Out 8），5.3.2 用槽位 0..9；1-based 的 “Line Out N” = 槽位 +1。详见 `docs/API.md` §10.1。
- **输入内容模式**（auto / 2.0 / multichannel）：读 `/run/tspi/input-content.json`（tspi-upmix 检测器写）显示当前判定，
  并可写 `/etc/tspi-audio/input-mode.json` 覆盖；优先级 override > detector > auto，auto 默认直通、检测到 2.0 才上混；
  H16 已上混/多声道时禁止二次上混。不改 channelmap v2 schema（v2.1 再正式加字段）。
- **remap**：可选 remap preset 一键生成，也可在矩阵里逐格增删输入源（多源求和、每源 gain/invert/mute）；保存前物化成显式
  output.roles[].sources（不再依赖 preset 字符串），与 audio-stack“sources 是最终矩阵、preset/routes 只用于生成”的约定一致。
- **每角色校准**：输出通道（支持多通道 fan-out）、enable/mute、gain、delay、polarity、HP/LP（LinkwitzRiley，阶数 2/4/6/8）、
  PEQ（最多 32 段；v1.1 `type` = peaking/low_shelf/high_shelf，UI 每行下拉 + 按 type 统计显示；peaking 的 q ∈ [0.1, 10]，shelf 的 q 是 RBJ slope S 必须在 [0.1, 1.0]，板端渲染 `slope=12*S`）、FIR、link_to；每角色测试音/扫频。
- **低音管理**：bass_management enabled 后由渲染器自动生成 SUB 输入（LFE+redirect）并接管 mains HP；SUB 角色的 sources 必须为空；
  可配 lfe_role/lfe_to/redirect sources/targets/lfe_gain/sub_gain/mains HP/sub LP。
- **FIR**：上传 WAV/txt/raw（≤16MiB），解析采样率/taps/群延迟；推荐 256–2048，>4096 实验性、>8192 强警告；采样率必须与模式一致，否则 validate/apply 拒绝（按 48k/96k 分 profile）。
- **profiles**：保存/加载/删除/导入/导出（按输入布局×输出布局命名）；加载只写 active，不自动 apply。
- **首启向导**：probe → 选输入/输出布局 → 生成显式草稿 → 编辑 → 保存/应用；D4 下 apply 在模式 off 时只校验/渲染/持久化，不启动 CamillaDSP。
- **H16 提示**：16ch 容器顺序 1 L/2 R/3 C/4 LFE/5 Ls/6 Rs/7 Lrs/8 Rrs/9-10 Lw/Rw(旧)/11-12 Ltf/Rtf/13-14 空/15-16 Ltr/Rtr；
  7.1.4 实际 12 路、5.1.2 实际 8 路；AES67/Dante 每流 ≤8ch，12ch 需 2 流；SDP 只给通道数不报角色；Pure bug 需每次开机确认。
- **v2.1（ART-like per-route FIR 矩阵）**：字段草案已记录（sources[].fir{path,samplerate,taps,band_limit,sparse}、roles[].limiter/max_gain_db、CPU/群延迟估算），
  页面留了占位说明；真正的矩阵 UI 等 audio-stack benchmark 结果后实现。

### 电源
重启/关机默认关闭；`[system] allow_reboot/allow_shutdown = true` 且页面输入确认短语（`REBOOT` / `SHUTDOWN`）后执行
`systemctl reboot/poweroff`。mock 模式只记录动作。

## 生产部署（Debian 12 / 泰山派3M）

### 1. 构建 aarch64 二进制

在 Linux x86_64 构建机（Debian/Ubuntu）上交叉编译：

~~~bash
sudo apt install -y gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
cd projects/tspi-audio-dsp/web
./scripts/cross-build.sh
# 产物：target/aarch64-unknown-linux-gnu/release/tspi-audio-web
~~~

也可以在 aarch64 板端/同架构容器中直接构建：

~~~bash
cargo build --locked --release
# 产物：target/release/tspi-audio-web
~~~

> 本工程是纯 Rust（无 C 依赖、无 OpenSSL/sqlite），目标 `aarch64-unknown-linux-gnu` 动态链接 glibc
> （Debian 12 = glibc 2.36，base 自带 libc6/libgcc-s1）。不需要 musl 工具链。
> 构建产物请留在普通磁盘（不要放 `R:` 等易失内存盘；也不要硬编码本机路径）。

### 2. 安装到设备

~~~bash
sudo ./scripts/install-device.sh --binary target/aarch64-unknown-linux-gnu/release/tspi-audio-web
# 等价手工步骤：
#   install -m 0755 tspi-audio-web /usr/local/bin/
#   install -d -m 0750 /etc/tspi-audio-web
#   install -m 0600 deploy/config.toml.example /etc/tspi-audio-web/config.toml
#   install -m 0644 deploy/tspi-audio-web.service /etc/systemd/system/
#   systemctl daemon-reload && systemctl enable --now tspi-audio-web
~~~

安装后：

~~~bash
sudo systemctl status tspi-audio-web --no-pager
sudo journalctl -u tspi-audio-web -b -f          # 首次启动会打印生成的 token
# 或随时找回 token：
sudo tspi-audio-web --config /etc/tspi-audio-web/config.toml --print-token
# 浏览器：http://<设备IP>/（默认 80；如被占用回退 8088）
~~~

### 3. OEM 镜像预装（rootfs overlay）

~~~bash
sudo ./scripts/install-device.sh \
  --binary target/aarch64-unknown-linux-gnu/release/tspi-audio-web \
  --rootfs /path/to/debian12-rootfs
~~~

脚本会安装二进制/配置/unit，并在 rootfs 内创建
`/etc/systemd/system/multi-user.target.wants/tspi-audio-web.service` symlink（不执行 systemctl），
首次开机由 systemd 启动并生成 token。

### 4. systemd

unit 内容见 `deploy/tspi-audio-web.service`：

- `tspi-audio-web.service`，`User=root`（需要 systemctl/journalctl 管理音频服务）；
- `ExecStart=/usr/local/bin/tspi-audio-web --config /etc/tspi-audio-web/config.toml`；
- `After=network-online.target`，`WantedBy=multi-user.target`，`Restart=on-failure`；
- 日志走 journald；Web 服务常开，不随音频模式起停；Web 启动不会自动启动音频（D4）。

### 5. 依赖与端口

二进制运行时：`libc6`（2.36）、`libgcc-s1`（Debian 12 base 自带）。

外部命令（按功能按需安装）：

| 包 | 用途 |
| --- | --- |
| systemd | systemctl / journalctl（必需） |
| alsa-utils | `aplay -l` 与测试音（建议） |
| iproute2 | `ip -j addr show` 总览网络（建议） |
| linuxptp | `pmc`（PTP 页面 / 方案 B2 需要） |

音频栈组件由 audio-stack 提供：CamillaDSP、aes67-linux-daemon、Inferno + Statime、`/usr/local/bin/tspi-mode`。

端口：**TCP 80**（默认）：局域网直接 `http://<设备IP>/`，入站放行 80 即可；端口 <1024 需要 root
（systemd unit 已 `User=root`）。如 80 被占用，改 `config.toml` 的 `listen = "0.0.0.0:8088"` 回退并放行 8088。
服务只访问本地 `ws://127.0.0.1:1234`、systemd、journald，无外网出站依赖。AES67 daemon 自带 Web UI 默认 8080，两者不冲突。

### 6. HTTPS（可选）

默认 HTTP 明文，仅在可信局域网使用。若需 HTTPS，建议在前面加 Nginx/Caddy 反代终止 TLS，
并在 `config.toml` 设置 `cookie_secure = true`；反代需转发 `/`、`/api/*`、`/healthz` 与 WebSocket 无关
（本服务不提供 websocket，CamillaDSP 的 websocket 仅本地访问）。

## aarch64 构建验证（task-5，已产出）

在构建 VM（Ubuntu 22.04.5 x86_64，SSH 127.0.0.1:2223，用户 builder，Debian12 aarch64 sysroot）上完成交叉编译：

~~~bash
cd projects/tspi-audio-dsp/web
bash scripts/cross-build.sh          # 52.73s（含 channelmap v2 对齐）
~~~

结果与校验（**2026-11-16 23:31 4.1.2 布局对齐重建**）：

| 项目 | 结果 |
| --- | --- |
| 产物 | `dist/tspi-audio-web`，4,539,776 bytes（4.33 MiB），stripped |
| 架构 | ELF 64-bit LSB pie executable, ARM aarch64；interpreter `/lib/ld-linux-aarch64.so.1`；BuildID `0da04a74…` |
| 依赖 | libgcc_s.so.1 / libm.so.6 / libc.so.6（Debian12 base 自带；最高 GLIBC 符号 2.34 ≤ 2.36） |
| SHA256 | `83ee5da2e79630c4f302aa4999e5abe72f0eb2b2a97ddd3abd81b562f0a9a081`（含 4.1.2 = FL0/FR1/SL2/SR3/HFL4/HFR5/SUB1=7 与 presets 防漂移；上一版 `f2c02b1b…` 作废） |
| cargo test | 124 单元 + 37 api + 2 presets_drift = 163 全绿（0 failed，Linux x86_64 构建机） |
| qemu 冒烟 | `--version` → 0.1.0；**默认监听 `0.0.0.0:80`**（root 不传 `-l`）：`/healthz`=200、无 token `/api/overview`=401、`/api/channelmap`=200；显式 `--listen 127.0.0.1:8088` 回退 `/healthz`=200；**4.1.2 = {FL:[0],FR:[1],SL:[2],SR:[3],HFL:[4],HFR:[5],SUB1:[7]}**（preset 与 draft 一致，validate valid）；high_shelf q=1.0 通过，q=1.5 拒绝，peaking q=15 仍拒绝 |
| rootfs 安装 | `install-device.sh --rootfs <rootfs> --dry-run` 完成；默认配置模板安装为 `listen = "0.0.0.0:80"`；沙箱安装验证二进制 0755、config 0600、unit 0644、enable symlink 正确 |

> 上一版（2026-11-16 21:15 `f2c02b1b…`）已作废：早于 4.1.2 布局对齐与防漂移测试。
> 更早（19:19 `3554eb2c…` / 17:32 `45dc0946…` / 11-15 `eba779b5…`）均已作废。
> 板端 `/etc/tspi-audio-web/config.toml` 的 `listen="0.0.0.0:80"` 已由 Lead 先行验证；端口变化不影响 token/auth/cookie/CSRF。
> 板端 `tspi-channelmap`（Python）的 type/slope 支持由 audio-stack/upmix-dev 并行改，与本二进制配套发布。

构建机上的交接路径：

- 产物：`/home/builder/audio-build/release/bin/tspi-audio-web`
- 交接包：`/home/builder/audio-build/proj/tspi-audio-web/`（二进制 + SHA256SUMS + unit + config 模板 + 安装脚本 + README/API + build/verify 日志）
- 源码/构建目录：`/home/builder/tspi-audio-dsp/web/`

OEM 预装用法与验收命令见 [deploy/OEM-HANDOFF.md](deploy/OEM-HANDOFF.md)；构建/验证原始记录见 `dist/INSTALL-MANIFEST.md`、
`dist/build.log`、`dist/verify.log`。

## 交叉编译与 crates.io 慢的处理

- 目标 triple：`aarch64-unknown-linux-gnu`；链接器 `aarch64-linux-gnu-gcc`（脚本自动探测，
  也可显式 `export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=...`）。
- 构建目录默认 `target/`；可用 `CARGO_TARGET_DIR` 指定，但不要指向易失内存盘。
- 若 crates.io 慢，可配置 rsproxy 镜像（在 `~/.cargo/config.toml`，不要写进仓库）：

~~~toml
[source.crates-io]
replace-with = "rsproxy-sparse"

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy]
index = "https://rsproxy.cn/crates.io-index"
~~~

然后 `cargo fetch` / `cargo build` 会走镜像；首次构建需联网，之后可离线（`cargo build --offline`）。

## 开发与测试

~~~bash
cargo fmt
cargo clippy --all-targets
cargo test                      # 单元 + 路由/集成测试（mock，无需设备）
TSPI_MOCK=1 cargo run -- --mock # 本地 UI 验证
~~~

当前测试覆盖（163 个用例：124 单元 + 39 路由/集成）：

- 单元：配置解析/首启 token、认证/CSRF/会话、systemd 状态解析、pmc/ptp4l.conf 解析与回滚、
  /proc 解析、CamillaDSP WS 回复解析/YAML 校验/配置切换、ALSA/PCM/hw_params 解析、模式状态机/脚本退出码。
- 路由集成（tower `ServiceExt`）：healthz/鉴权/CSRF/登录注销、总览、CamillaDSP/音量/配置切换/非法 YAML、
  PTP 状态与配置、AES67/Inferno、ALSA/测试音、CueMix 状态/直通静音/回滚注入、音频格式范围/回滚、电源开关/确认短语、
  运行模式双向切换（off→aes67→dante→off）、互斥 409、失败回滚、持久化展示与可选恢复、静态资源与安全头、
  声道分配（含 4.1.2=FL0/FR1/SL2/SR3/HFL4/HFR5/SUB1=7 的 API 视图断言）。
- 防漂移（`tests/presets_drift.rs`）：web/assets/layout-presets.yml 与板端
  audio/overlay/etc/tspi-audio/layout-presets.yml 的 output role→outputs、input role→channel、
  remap preset 全量比较；不漂移才过，并单独断言 4.1.2 的 MOTU 真相。

测试的临时目录建在工程目录内（避免本机 TEMP 指向易失内存盘时的权限问题）。

## 已知边界（一期）

- CamillaDSP 4.x websocket **没有独立 xrun 计数命令**；页面 xrun 来自 journalctl 日志关键词 + StopReason 推断。
- AES67 daemon 没有公开 PTP 锁定状态接口；B1 下锁定状态按 daemon 日志 / `tspi-mode status --json` 的 `ptp_lock` 展示。
- Inferno 上游无统一配置文件；页面只读展示部署时指定的路径。
- 位深/声道当前只读展示；一期只允许改采样率/chunksize/target_level。
- 声道分配一期：每角色单输入 FIR + 静态增益 remap；**ART-like 全 MIMO（输入×输出每格 FIR 矩阵）等 audio-stack v2.1 schema**
  到位后再加矩阵页（每格 FIR 上传/启用/增益、CPU/延迟预估、系数采样率校验、profile 导入导出；超长 FIR 只提示不硬砍）。
- H16：输入布局仅 5.1/7.1/5.1.2/5.1.4/7.1.2/7.1.4；16ch 顺序 1 L/2 R/3 C/4 LFE/5 Ls/6 Rs/7 Lrs/8 Rrs/9-10 Lw/Rw(旧)/
  11-12 Ltf/Rtf/13-14 空/15-16 Ltr/Rtr；7.1.4 实际 12 路、5.1.2 实际 8 路；AES67/Dante 每 flow ≤8ch，12ch 需 2 流；
  SDP 只报通道数不报角色；H16 Pure bug（待机再开机回 Multi CH）需每次开机确认。
- 认证为单 token 模式（无多用户/角色）；默认无 HTTPS，依赖内网 + token + CSRF。
- CueMix 页只写 Main 音量和直通推子。EQ/门/压缩/混响只读，设备密码（RSA-OAEP）、固件、单格矩阵编辑都不做。
  `kMainTrim` 写入上限暂定为 80（−80 dB），真机合法范围、`k48V` 下标含义、已设密码的设备都还要在板上确认。协议说明见 `docs/MOTU-CUEMIX.md`。
- mock 模式不会执行任何真实系统命令，与真机行为在服务状态细节上可能有差异（以真机验收为准）。

## 安全说明

- 默认 token 首启随机生成并打印到 journald；配置文件 0600，目录 0750；请勿把 token 提交到仓库。
- 浏览器会话 Cookie：HttpOnly + SameSite=Strict；写操作 CSRF；登录失败限速；常量时间 token 比较。
- 响应安全头：CSP（仅同源脚本）、X-Frame-Options DENY、nosniff、no-referrer、no-store。
- systemd 以 root 运行（管理其他服务所需）；加固项见 unit（NoNewPrivileges 等）。
- 前端无外网 CDN、无内联脚本，静态资源编译期内嵌。

## 故障排查

| 现象 | 处理 |
| --- | --- |
| 忘了 token | `sudo tspi-audio-web --config /etc/tspi-audio-web/config.toml --print-token` 或 `journalctl -u tspi-audio-web -b | grep -i token` |
| 80 被占用 / 无法绑定 | 默认 80；如被占用，把 `config.toml` 的 `listen` 改为 `0.0.0.0:8088` 回退后重启 unit，或排查占用进程（8080 是 AES67 daemon，勿用）；Linux 非 root 无法绑定 <1024，生产用 systemd 的 root 单元 |
| 页面显示 CamillaDSP 不可达 | 确认 camilladsp 以 `-p 1234` 启动、`[camilladsp] ws_url` 正确；不影响其他页面 |
| PTP 页面 offset 为空 | 确认当前模式与 PTP 来源（B1 不运行 ptp4l 属正常；B2/Dante 看对应服务与日志） |
| 模式切换失败 | 页面查看步骤与 `last_error`；内置编排失败会自动回滚；检查 tspi-mode 退出码含义（API.md `6） |
| 电源按钮灰/403 | 默认关闭；改 `[system] allow_reboot/allow_shutdown` 后重启 unit |
| mock 下不执行命令 | 设计如此（防止误操作）；真机行为需在设备上验收 |

## 许可

工程内代码用于泰山派音频 DSP 设备固件配套；第三方协议遵循其上游项目（CamillaDSP/linuxptp/AES67 daemon/Inferno）各自许可。
