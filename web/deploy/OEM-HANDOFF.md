# OEM 预装交接说明（tspi-audio-web · Debian 12 / RK3576）

> 给 firmware-builder / 镜像构建流水线。对应任务 task-5（aarch64 交叉编译 + OEM 交接）。
> D4 口径：默认上电开机、Web 常开、音频 off、手动切模式、不自动恢复音频。

## 1. 文件与安装布局

| 源文件（交付包） | 目标路径 | 权限 |
| --- | --- | --- |
| `tspi-audio-web`（aarch64 ELF） | `/usr/local/bin/tspi-audio-web` | 0755 root:root |
| `tspi-audio-web.service` | `/etc/systemd/system/tspi-audio-web.service` | 0644 root:root |
| `config.toml.example` | `/etc/tspi-audio-web/config.toml` | 0600 root:root（目录 0750） |
| （enable symlink） | `/etc/systemd/system/multi-user.target.wants/tspi-audio-web.service` → unit | symlink |

- 单二进制：前端页面、CSS/JS、favicon、测试音 WAV 全部编译期内嵌，**不需要额外 assets 目录**。
- 架构：aarch64-unknown-linux-gnu，动态链接 glibc（Debian 12 = 2.36；实测最高符号 GLIBC_2.34）；
  需要 `libc6`、`libgcc-s1`（Debian 12 base 自带），无 OpenSSL/sqlite/cert/python 依赖。
- 已知产物 SHA256：见 `SHA256SUMS`（当前：`eba779b5d396f438c4e87b780c00292b384348f467f85574d27173fff8372d35`，4,281,728 bytes；含 input-mode + 首设管理密码/Argon2id；token 恢复兜底）。

## 2. 一条命令预装进 rootfs

~~~bash
# 在构建机/镜像流水线（root）：
sudo ./install-device.sh --binary ./tspi-audio-web --rootfs /path/to/debian12-rootfs
# 只检查契约、不写盘：
./install-device.sh --binary ./tspi-audio-web --rootfs /path/to/rootfs --dry-run
~~~

脚本行为：安装二进制/配置/unit → 在 rootfs 内创建 `multi-user.target.wants` symlink 表达 enable，
**不执行 systemctl**（镜像构建阶段无运行中的 systemd）。残留配置不会被覆盖（存在则跳过 config 写入）。

## 3. 首次启动、管理密码与 token

- **首次打开 Web**：若 `/etc/tspi-audio-web/auth.json` 不存在 → 显示“设置管理密码”页；密码 Argon2id 哈希写 auth.json（0600），
  设置后立即登录；已配置后不再允许 setup。OEM 镜像不生成默认固定密码。
- 密码登录：登录页输入管理密码；token 保留为恢复/CLI 兜底（`--print-token` / `X-Auth-Token`），设置密码后仍有效。
- 首次启动时若 `/etc/tspi-audio-web/config.toml` 缺失 → 自动按模板生成，随机生成 32 hex token，
  写入 0600 文件并打印到 journald（横幅 "已生成新的访问 token"）。
- 若预置模板（token = CHANGE_ME）→ 首次启动自动轮换为新 token 并打印。
- 查找 token：`journalctl -u tspi-audio-web -b | grep -i token`
  或 `sudo tspi-audio-web --config /etc/tspi-audio-web/config.toml --print-token`。
- **token 双文件同步（根修）**：服务启动时会把当前 token 写入环境变量 `TSPI_WEB_TOKEN_FILE`
  （默认 `/etc/tspi/web-token`，0600），与 first-boot 的 token 文件保持一致，避免登录 401。
- 修改 token 后需 `systemctl restart tspi-audio-web`（配置仅启动加载）。
- 如需每台设备固定 token：可在镜像构建时用随机值替换 config 模板 token 行。
- Web 登录：`http://<设备IP>/`（默认端口 80，无需记端口；如 80 被占用，按 §4 改 `listen` 回退 8088），输入 token。

## 4. systemd 与运行方式

- unit：`tspi-audio-web.service`；`User=root`（需要 systemctl/journalctl 管理 camilladsp/ptp4l/aes67-daemon/inferno）；
  `ExecStart=/usr/local/bin/tspi-audio-web --config /etc/tspi-audio-web/config.toml`；
  `WantedBy=multi-user.target`、`Restart=on-failure`；日志 journald。
- **Web 服务常开**，与音频模式（dante/aes67/off）解耦；开机即启动。
- 音频服务仍由 audio-stack 的 `tspi-mode` 控制；D4：默认 boot 时音频 off，Web 不会自动调用 tspi-mode；
  用户在 Web「运行模式」页手动切换。
- 端口：TCP 80（默认；入站放行 80 即可；端口 <1024 由 root 的 systemd unit 绑定；如 80 被占用改成 8088 回退并放行 8088）；仅本地访问 ws://127.0.0.1:1234、systemd、journald，无外网出站依赖。

## 5. 运行依赖（镜像预装建议）

| 包 | 用途 |
| --- | --- |
| systemd | systemctl / journalctl（必需） |
| alsa-utils | `aplay -l`、测试音（建议） |
| iproute2 | `ip -j addr show` 总览网络（建议） |
| linuxptp | `pmc`（PTP 页面 / AES67 方案 B2 需要） |

音频栈：CamillaDSP、aes67-linux-daemon、Inferno + Statime、`/usr/local/bin/tspi-mode`（audio-stack 提供）。
若 `tspi-mode` 不存在，Web 的模式切换自动回退到内置编排（systemctl + CamillaDSP 配置重载）。

## 6. 验收命令（镜像构建后/首启后）

~~~bash
# 1) 文件与权限
ls -l /usr/local/bin/tspi-audio-web /etc/systemd/system/tspi-audio-web.service
stat -c '%a %U:%G %n' /etc/tspi-audio-web/config.toml
readlink /etc/systemd/system/multi-user.target.wants/tspi-audio-web.service

# 2) 服务状态/端口
systemctl is-enabled tspi-audio-web && systemctl is-active tspi-audio-web
ss -ltnp | grep ':80\b'

# 3) HTTP
curl -s http://127.0.0.1/healthz
# {"status":"ok","version":"0.1.0",...}

# 4) token（首启后）
journalctl -u tspi-audio-web -b | grep -i token
~~~

## 7. 升级/回滚

- 升级：停止服务 → 替换 `/usr/local/bin/tspi-audio-web`（记录 sha256）→ 启动；配置文件与 state.json 保留。
- 回滚：换回旧二进制即可；配置文件向后兼容（未知字段忽略）。
- 卸载（可选）：`systemctl disable --now tspi-audio-web`，删除 unit/二进制；`/etc/tspi-audio-web` 保留 token 与状态。

## 8. 备注

- 本服务不修改音频模式默认行为；D4 已内置（`restore_on_start=false`）。
- 本服务写操作范围：`/etc/tspi-audio-web/`（state.json、token 轮换）、`/etc/camilladsp/`（配置切换/格式修改，先备份）、
  `/etc/ptp4l.conf`（PTP 配置，先备份）；均在 Web 写操作时发生，默认部署无写操作。
- 构建环境与验证记录见同目录 `INSTALL-MANIFEST.md` / `build.log` / `verify.log`（在 Web 工程 dist/ 下）。
