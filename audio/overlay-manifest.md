# overlay-manifest.md — 进镜像文件清单（来源/权限/所属包/默认启停）

> 执行位注意：从 Windows 拷到 Linux（zip/共享目录/scp 后）可能丢失 +x；`install-into-image.sh` 复制后会强制修正：`usr/local/bin/tspi-*` 与 `usr/local/lib/tspi/*` 统一 755，`etc/**`、unit、udev、UCM 等文本统一 644。
>
> 交付契约：`overlay/` 是 **rootfs 根** 开始的文件树，由 `install-into-image.sh` 原样复制（保持 root:root、权限），
> 再叠加 `release/` 里的构建产物与 `release/debs` 离线包。每新增/删除文件必须同步本表。

## 1. 默认启停语义（重要）

| 对象 | 状态 | 说明 |
| --- | --- | --- |
| `tspi-audio.target` 及各音频 service | **enabled（有 .wants 符号链接），但开机不 active** | 每个 unit 带 `ConditionPathExists=/run/tspi/...` 守卫；`/run` 是 tmpfs，开机时 marker 不存在 → 条件不满足 → 跳过启动。由 `tspi-mode` 写 marker 后显式 `systemctl start`。 |
| `tspi-boot-mode.service` | **disabled（默认不 enable）** | opt-in：安装时加 `--enable-boot-mode` 才 enable；启用后开机读 `/etc/tspi/last-mode` 调 `tspi-mode --restore`。 |
| B2 的 GM 时钟链（`tspi-ptp4l-master` / `tspi-phc2sys` / `tspi-phc-usrvclock`） | 随 `tspi-audio.target` enabled，但开机不 active | 三条 unit 同样被 `ConditionPathExists=/run/tspi/active-aes67-master` 守卫；只有 `tspi-mode b2`（或 `TSPI_AES67_PTP_ROLE=master`）写 marker 后才启动。 |
| 二进制/配置/UCM | 就位 | 不运行；由上面的服务/脚本使用。 |
| 安全状态 | **off** | 首次开机无任何音频服务运行（安全默认）；由 web-manager/人工执行 `tspi-mode dante|aes67|b2`。 |

## 2. overlay 文件（随本目录提交）

| 路径（rootfs 内） | 类型 | 来源 | 权限/属主 | 说明 |
| --- | --- | --- | --- | --- |
| /usr/local/bin/tspi-mode | 脚本 | 本方案自写 | 755 root:root | 模式切换/状态入口（web-manager 唯一调用入口） |
| /usr/local/bin/tspi-ptp-status | 脚本 | 本方案自写 | 755 root:root | PTP 状态（statime 观测 socket / daemon API） |
| /usr/local/lib/tspi/lib.sh | 脚本 | 本方案自写 | 644 root:root | 公共 shell 函数 |
| /usr/local/lib/tspi/ptp-status-hook | 脚本 | 本方案自写 | 755 root:root | aes67-daemon `ptp_status_script` 回调，写 /run/tspi/ptp-lock |
| /usr/local/lib/tspi/ucm-backport-mk5.sh | 脚本 | 本方案自写 | 755 root:root | MOTU mk5 UCM 回移（可选，幂等） |
| /usr/local/lib/tspi/phc-usrvclock | 脚本 | 本方案自写（由 B2 实测脚本 `evidence/b2-ptp-gm/board-scripts/phc_usrvclock.py` 收敛） | 755 root:root | B2：把网卡 PHC 导出为 usrvclock overlay（Inferno `CLOCK_PATH=/tmp/ptp-usrvclock`）；`--probe` 自检 |
| /usr/local/bin/README.md、/usr/lib/aarch64-linux-gnu/alsa-lib/README.md | 文档 | 本方案自写 | 644 | 说明二进制由 install 注入 |
| /etc/default/tspi-audio | 配置 | 本方案自写 | 644 root:root | 运行期默认值（采样率/chunksize/超时/网卡/PTP 角色） |
| /etc/asound.conf | 配置 | 本方案自写 | 644 root:root | `pcm.inferno`（Dante 虚拟声卡） |
| /etc/camilladsp/dante.yml | 模板 | 本方案自写（CamillaDSP v4 语法） | 644 | Dante 模式（96k/24，H16 16ch 入 → 5.3.2 十路，22ch 补零）→ tspi-mode 渲染到 /run/tspi/camilladsp.yml |
| /etc/camilladsp/aes67.yml | 模板 | 本方案自写 | 644 | AES67 模式（48k/24，H16 16ch 入 → 5.3.2 十路，22ch 补零） |
| /etc/camilladsp/coeffs/README.md | 文档 | 本方案自写 | 644 | FIR 系数目录说明（业务系数由用户提供） |
| /etc/statime/inferno-ptpv1.toml | 模板 | 基于 teodly/statime `inferno-ptpv1.toml` + 本方案观测段 | 644 | PTPv1 follower；`__IFACE__` 由 tspi-mode 渲染 |
| /etc/ptp4l-master-aes67.conf | 配置 | 本方案自写（AES67/Dante 互操作口径） | 644 | 仅 B2/B3 用 |
| /etc/aes67-daemon/daemon.conf | 模板 | 基于 bondagit/aes67-linux-daemon `systemd/daemon.conf` + 本方案字段 | 644 | `__IFACE__` 渲染；ptp_status_script 指向 hook |
| /etc/security/limits.d/95-tspi-audio.conf | 配置 | 本方案自写 | 644 | RT 权限（rtprio 99 / memlock unlimited） |
| /etc/modprobe.d/tspi-audio.conf | 配置 | 本方案自写 | 644 | 固定 ALSA card id：motu / ravenna |
| /etc/udev/rules.d/95-motu-ultralite-mk5.rules | 配置 | 本方案自写 | 644 | MOTU 07fd:000c 权限与稳定别名 |
| /etc/tmpfiles.d/tspi-audio.conf | 配置 | 本方案自写 | 644 | /run/tspi、/var/log/tspi、/var/lib/tspi 等目录 |
| /etc/systemd/system/tspi-audio.target | unit | 本方案自写 | 644 | 总目标；条件 /run/tspi/mode |
| /etc/systemd/system/tspi-statime-inferno.service | unit | 本方案自写 | 644 | PTPv1 follower（Dante） |
| /etc/systemd/system/tspi-aes67-daemon.service | unit | 本方案自写（参考上游 unit，去掉 User=aes67-daemon） | 644 | AES67 daemon |
| /etc/systemd/system/tspi-ravenna-lkm.service | unit | 本方案自写 | 644 | 仅模式 B modprobe/rmmod LKM |
| /etc/systemd/system/tspi-camilladsp.service | unit | 本方案自写 | 644 | DSP 引擎（环境变量注入 INFERNO_*） |
| /etc/systemd/system/tspi-ptp4l-master.service | unit | 本方案自写 | 644 | B2（`tspi-mode b2`）：ptp4l masterOnly GM |
| /etc/systemd/system/tspi-phc2sys.service | unit | 本方案自写 | 644 | B2：phc2sys -O 37（网卡 PHC 跑 TAI） |
| /etc/systemd/system/tspi-phc-usrvclock.service | unit | 本方案自写 | 644 | B2：PHC→usrvclock 桥（Inferno 媒体时钟） |
| /etc/systemd/system/tspi-boot-mode.service | unit | 本方案自写 | 644 | opt-in 开机恢复 |
| /usr/share/alsa/ucm2/USB-Audio/MOTU/UltraLite-mk5.conf | 配置 | 上游 alsa-ucm-conf（commit 2bd2aca / v1.2.13） | 644 | MOTU mk5 UCM 回移（精简） |
| /usr/share/alsa/ucm2/USB-Audio/MOTU/UltraLite-mk5-HiFi.conf | 配置 | 本方案精简版（Syntax 6 兼容） | 644 | 只保留 Full 设备；CamillaDSP 不依赖 |

## 3. release 产物（install-into-image.sh 注入，不在 overlay 里提交）

| 目标路径（rootfs 内） | 来源 | 权限 | 所属组件 | 备注 |
| --- | --- | --- | --- | --- |
| /usr/local/bin/camilladsp | release/bin/camilladsp | 755 | CamillaDSP v4.1.3 | 官方 aarch64 资产或源码构建 |
| /usr/local/bin/statime | release/bin/statime | 755 | teodly/statime fork | PTPv1 follower |
| /usr/local/bin/inferno2pipe | release/bin/inferno2pipe | 755 | Inferno dev@9767558 | 可选工具 |
| /usr/local/bin/aes67-daemon | release/bin/aes67-daemon | 755 | bondagit/aes67-linux-daemon | 模式 B |
| /usr/lib/aarch64-linux-gnu/alsa-lib/libasound_module_pcm_inferno.so | release/lib/ | 644 | Inferno ALSA 插件 | 模式 A 必需 |
| /lib/modules/<kver>/extra/MergingRavennaALSA.ko | release/modules/ | 644 | bondagit/ravenna-alsa-lkm | 模式 B 必需；kver 必须与目标内核一致 |
| /usr/sbin/ptp4l, /usr/sbin/phc2sys, /usr/sbin/pmc | release/debs/linuxptp_*.deb | 755 | linuxptp 3.1.1-4 | 由 dpkg 安装 |
| 其他库（libasound2/libboost*/libavahi*/jq/netcat-openbsd 等） | release/debs/*.deb | - | Debian bookworm | dpkg 离线安装 |

## 4. 生成物（运行期，不随镜像）

| 路径 | 内容 | 生成者 |
| --- | --- | --- |
| /run/tspi/mode | dante / aes67 / b2 / off | tspi-mode |
| /run/tspi/state.json | 模式状态 + last_error（web-manager 可读） | tspi-mode |
| /run/tspi/config.json | 本次渲染的采样率/设备/通道/格式 | tspi-mode |
| /run/tspi/camilladsp.yml | 渲染后的 CamillaDSP 配置 | tspi-mode |
| /run/tspi/camilladsp.env | INFERNO_* + TSPI_CAMILLADSP_* 环境 | tspi-mode |
| /run/tspi/statime.toml / aes67-daemon.conf | 渲染后的组件配置 | tspi-mode |
| /run/tspi/ptp-lock | AES67 daemon PTP 状态（hook 写） | ptp-status-hook |
| /run/tspi/statime-observe.sock | statime 观测 JSON socket | statime |
| /tmp/ptp-usrvclock | usrvclock 时钟导出 socket（→Inferno）。dante=statime 写；b2=phc-usrvclock 写 | statime / phc-usrvclock |
| /run/tspi/b2-audio | B2 运行态变体（1=GM+Inferno+DSP，0=`--ptp-only` 仅 GM）；status 用 | tspi-mode |
| /etc/tspi/last-mode | 上次成功模式（供 boot-mode 恢复） | tspi-mode |
| /var/log/tspi/ | 预留日志目录 | tmpfiles |
| /etc/tspi-audio-manifest.txt | 安装清单（安装时生成） | install-into-image.sh |

## 5. 依赖的 Debian 包（离线 .deb，install-into-image.sh 安装）

见 `packages.list` 的 [runtime] 段；`release/debs/MANIFEST.txt` 记录实际版本与 sha256。

## 6. 构建产物与镜像的版本一致性校验

`release/MANIFEST.sha256`（build-all.sh 生成）覆盖 bin/lib/modules/debs；install 时写入
rootfs 的 `/etc/tspi-audio-manifest.txt`。产线可用这两个文件核对版本与哈希。
