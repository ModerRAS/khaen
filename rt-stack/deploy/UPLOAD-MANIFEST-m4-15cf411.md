# UPLOAD-MANIFEST — 板端上传清单（M4 / m4-15cf411-20261118，task-214）

> **现行 T71 板端上传依据（2026-11-18）**；取代 M2b（release-v2）上传口径。板端动作全部 **NOT_RUN**，必须在 T71 授权窗口内、按 `deploy/README.md` 流程执行。
> 上传包：`rt-stack/evidence/release-v3/artifact/tspi-deploy-m4-15cf411-20261118.tgz`（**1,139,793 B，sha256 `be6e1951130b7de0aeb345eeffffa5fb27b26d256eaa5020f5b2e4f906dbbb35`，79 files**；由 builder VM 上 Linux tar 打包，脚本执行位保留）
> 构建源：M4 代码冻结 **git 15cf411**（rt-stack 树）；aarch64 host/daemon 由 builder VM 交叉构建；bundle `m4-15cf411-20261118`（14 files）。

## 1. 上传包 → 板端目标（由板上脚本安装，不手工拷）

| 包内目录/文件 | 内容 | 板端去向（04/05 安装） |
| --- | --- | --- |
| `deploy/`（62 项） | `bin/00..08` + `make-release-bundle.sh`、`lib/`（common/check_window/check_elf/release_manifest/last_good/snapshot_to_json/irq_affinity）、`config/`（toml/env 模板、keeper/udev、irq pin、old-c-units、switch-window 示例）、`tools/`、`tests/`（dry-run/unit/rollback 检查）、`README.md`、`T71-SOUND-TEST-NOTES.md` | `/var/tmp/tspi-rt-stack-deploy/`（root 执行） |
| `service/`（9 项） | `khaen-rt-host.service`、`tspi-rt-daemon.service`、`tspi-rt-stack.target`、`tspi-irq-affinity.service`、`khaen-rt-host.service.d/90-safe-mute.{conf,env}.example`、`README.md` | `/etc/systemd/system/`（04 安装；target Wants/After host+daemon） |
| `release-bundle/`（20 项） | `bin/khaen-rt-host`、`bin/tspi-rt-daemon`、`units/`（6）、`etc/`（5 个 `*.example`）、`manifest.json`、`README.deploy.md`、`SHA256SUMS` | `/opt/tspi-rt-stack/releases/m4-15cf411-20261118/{bin,units,manifest.json,README.deploy.md}` + `/opt/tspi-rt-stack/current`（symlink） |
| `release-bundle/units/*.service|*.target` | host/daemon/target/irq-affinity + safe-mute drop-in | `/etc/systemd/system/`（04 安装） |
| `release-bundle/etc/*.example` | `rt-host.toml`、`rt-daemon.toml`、`rt-host.env`、`rt-daemon.env`、`irq-affinity.env` 模板 | `/etc/tspi-rt-stack/`（04 首次安装，**不覆盖**既有） |
| `deploy/config/{tspi-motu-48k-keeper.service,timer,95-tspi-motu-48k-keeper.rules,motu-48k-keeper,irq-affinity-pin}` | MOTU 空闲 48k keeper、udev 规则、IRQ 钉核 | `/etc/systemd/system/`、`/etc/udev/rules.d/`、`/usr/local/lib/tspi/`（04 安装） |
| `README-UPLOAD.md`、`SHA256SUMS` | 上传说明 + 包级 79 项校验和 | `/var/tmp/` 保留（校验用） |

## 2. 关键文件 sha256（完整 79 项见包内 `SHA256SUMS`；bundle 14 项见 `rt-stack/evidence/release-v3/ARTIFACTS.md` §2）

| 文件 | 大小(B) | sha256 |
| --- | ---: | --- |
| bin/khaen-rt-host（bundle） | 1176128 | `fb247696bd8ef5537359ddb832278c35333624fdd78bad78c13f3627610dd0c6` |
| bin/tspi-rt-daemon（bundle） | 1286384 | `804a8bcd94bc73cf8aff29bc459633abbee0cf55d50c512ad4215c410c577c18` |
| units/khaen-rt-host.service | 2487 | `2a1a836132b0abf4d101fbea6be9e0b0143db2cf6213dd9cfd7494e90a8f66de` |
| units/tspi-rt-daemon.service | 1873 | `c750f1405ee00dc151130c67a2b77cdd70f02d96428ece89fe904e52ef1618e3` |
| units/tspi-rt-stack.target | 1055 | `8948ef98470619ab6f491c03276d6a8cf740ca35219968d964fe578551fd9ca0` |
| units/tspi-irq-affinity.service | 1589 | `869073d6e2c3dc91429185ee160ca43adbb73a4a4442f6fc7835e052b0b13c90` |
| etc/rt-host.toml.example | 1751 | `9d251c07eafef33a09f0479807892b78e5790224bd30936547c5e89b7bd0e488` |
| etc/rt-daemon.toml.example | 552 | `47308b35fc45b065a22970a7271c458fe1801ebe9ec86c4d62a3b34411839034` |
| bundle tgz（可单独上传） | 1041790 | `602124b9cbb6097f21976f15c7409da8edc04db9067dd05486e34df1566c57b0` |

## 3. 上传与板端只读校验

```bash
# 上传（任何有 scp 的跳板/构建机）
scp tspi-deploy-m4-15cf411-20261118.tgz board:/var/tmp/
ssh board 'cd /var/tmp && tar xzf tspi-deploy-m4-15cf411-20261118.tgz'
# 包级校验（79 项）
cd /var/tmp && sha256sum -c SHA256SUMS
# bundle 语义校验（只读）
python3 deploy/lib/release_manifest.py verify release-bundle --require-arch aarch64   # files_checked=14, ok
python3 deploy/lib/check_elf.py release-bundle/bin/khaen-rt-host --require-arch aarch64
python3 deploy/lib/check_elf.py release-bundle/bin/tspi-rt-daemon --require-arch aarch64
```

## 4. 安装/切换（授权窗口内，root；唯一入口 05）

```bash
sudo ./deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/window.json \
     --bundle /var/tmp/release-bundle --allow-first-switch
# 结果证据：/var/lib/tspi-rt-stack/evidence/<ts>-05-atomic-switch/{stages.log,result.json,...}
```

> 内部顺序：00 snapshot → 01 pre-install（失败=零变更）→ 04 install（release/current + units + env/toml + disable irqbalance/IRQ pin + keeper，不 start）→ 01 完整 A–J → 03 retire-c → 02 设备独占 → start keeper/irq-affinity → enable/start target（等 host+daemon active）→ 06 verify（V1–V10）→ 07 last-good。失败只回自研 last-good 或 mute（G-no-C）。

## 5. 依赖与边界

- 板上需 `python3`（04/05/06 的 lib 检查器）、`systemctl`、`tar`；上传包不联网。
- **所有板端动作 NOT_RUN**：安装/systemd/enable/start/真实 ALSA/MOTU 播放/RT/IRQ 持久化/浏览器/回滚演练均未执行（见 `rt-stack/evidence/release-v3/NOT_RUN.md`）。
- 板端当前仍是旧 web dist `83ee5da2…`；M4 重打的 `web/dist/khaen-audio-web`（4,519,296 B `d458ae7b…`，与 M2b 同字节）**另行**按 `web/scripts/install-device.sh` 安装，不属本上传包。
- 禁止 CamillaDSP/旧 C unit 作任何回退；授权窗口 JSON 与 `audio_off/S1/soak` 口径见 `deploy/README.md` §1/§3。
