# UPLOAD-MANIFEST — 板端上传清单（release-v4 / m4-15cf411-20261118-v4，task-217）

> **现行 T71 板端上传依据（2026-11-18 F1–F5 修复后）**；取代 release-v3 的 `m4-15cf411-20261118` 上传包。
> 板端动作全部 **NOT_RUN**，必须在 T71 授权窗口内、按 `deploy/README.md` 流程执行。
> 修复依据：`rt-stack/evidence/board-window-20261118/deploy/FINDINGS.md` F1–F5；修复证据：`rt-stack/evidence/cursor-fix-deploy-f1f5/`；
> 重打包证据（包大小/sha/逐文件）：`rt-stack/evidence/release-v4/ARTIFACTS.md`。
>
> 上传包：`rt-stack/evidence/release-v4/artifact/khaen-deploy-m4-15cf411-20261118-v4.tgz`。
> **说明**：本清单在包内，包自身 sha256 记在包外（`artifact/*.sha256` 与 ARTIFACTS.md），避免自引用；校验以
> 包外 sha256 + 包内 `SHA256SUMS`（sha256sum -c）+ 板端 `test -x` 为准。
>
> 构建源：M4 代码冻结 **git 15cf411**（aarch64 二进制与 release-v3 **字节相同**，本轮未改 Rust 源码）；
> deploy/service/打包工具为 2026-11-18 F1–F5 修复版。bundle `m4-15cf411-20261118-v4`（14 files；tgz
> 1,043,267 B sha256 `6b83f0c1a6332a14335669de5baead24010e6c5daf86815cedc6fd3477e41f5b`）。

## 1. 上传包 → 板端目标（由板上脚本安装，不手工拷）

| 包内目录/文件 | 内容 | 板端去向（04/05 安装） |
| --- | --- | --- |
| `deploy/` | `bin/00..08` + `make-release-bundle.sh` + **`make-deploy-package.sh`（新）**、`lib/`（common/check_window/check_elf/release_manifest/last_good/snapshot_to_json/irq_affinity/**release_tar.py 新**）、`config/`、`tools/`、`tests/`、README、UPLOAD-MANIFEST | `/var/tmp/khaen-rt-stack-deploy/`（root 执行） |
| `service/` | host/daemon/target/irq-affinity units + host.service.d/90-safe-mute.{conf,env}.example + README | `/etc/systemd/system/`（04 安装；target Wants/After host+daemon） |
| `release-bundle/`（16 文件） | `bin/khaen-rt-host`、`bin/khaen-rt-daemon`、`units/`（6）、`etc/`（5 个 `*.example`）、manifest.json、README.deploy.md、SHA256SUMS | `/opt/khaen-rt-stack/releases/m4-15cf411-20261118-v4/{bin,units,manifest.json,README.deploy.md}` + `current`（symlink） |
| `README-UPLOAD.md`、`SHA256SUMS` | 上传说明 + 包级逐文件校验和 | `/var/tmp/` 保留（校验用） |

## 2. 关键文件 sha256（完整清单见包内 `SHA256SUMS`；bundle 14 项见 `rt-stack/evidence/release-v4/ARTIFACTS.md` §2）

| 文件 | 大小(B) | sha256 |
| --- | ---: | --- |
| bin/khaen-rt-host（bundle） | 1176128 | `fb247696bd8ef5537359ddb832278c35333624fdd78bad78c13f3627610dd0c6` |
| bin/khaen-rt-daemon（bundle） | 1286384 | `804a8bcd94bc73cf8aff29bc459633abbee0cf55d50c512ad4215c410c577c18` |
| units/khaen-rt-host.service（**F3/F4 修改**） | 3594 | `1ddda0fb07e2b60e326b0a430336238f8ca65b7d58373b448923ccb8464be708` |
| units/khaen-rt-daemon.service（**F3 修改**） | 2522 | `7d3028ab244fe86c2fcf54bd43fbede882a51c1e3cd0c6ea2d766dcebdde9182` |
| units/90-safe-mute.conf.example（**F5 注释**） | 1513 | `7b1ae91920a678d1953dbfc5e1a7f29a803469e4eea04ddc1833c61d114aa55e` |
| units/khaen-rt-stack.target | 1055 | `8948ef98470619ab6f491c03276d6a8cf740ca35219968d964fe578551fd9ca0` |
| units/khaen-irq-affinity.service | 1589 | `869073d6e2c3dc91429185ee160ca43adbb73a4a4442f6fc7835e052b0b13c90` |
| etc/rt-host.toml.example | 1751 | `9d251c07eafef33a09f0479807892b78e5790224bd30936547c5e89b7bd0e488` |
| bundle tgz（可单独上传） | 1043267 | `6b83f0c1a6332a14335669de5baead24010e6c5daf86815cedc6fd3477e41f5b` |

> 与 release-v3 比：两 bin 与 5 个 etc 模板 sha256 不变；units 中 host/daemon/90-safe-mute.conf.example 因 F3/F4/F5 变更，
> target/irq-affinity/90-safe-mute.env.example 不变。逐项对比见 `rt-stack/evidence/release-v4/ARTIFACTS.md`。

## 3. 上传与板端只读校验

```bash
# 上传（任何有 scp 的跳板/构建机）
scp khaen-deploy-m4-15cf411-20261118-v4.tgz board:/var/tmp/
ssh board 'cd /var/tmp && tar xzf khaen-deploy-m4-15cf411-20261118-v4.tgz'
# 包级校验（sha256sum -c 覆盖包内全部文件）
cd /var/tmp && sha256sum -c SHA256SUMS
# F1 执行位自检（打包侧已断言；板端再验，防中转丢位）
test -x deploy/bin/05-atomic-switch.sh && test -x deploy/bin/03-retire-c.sh && echo SCRIPT_EXEC_OK
# 兜底（正常不应走到；仅当中转/解包工具丢执行位）
chmod +x deploy/bin/*.sh deploy/tools/*.sh deploy/tests/*.sh \
         deploy/config/irq-affinity-pin deploy/config/motu-48k-keeper
# bundle 语义校验（只读）
python3 deploy/lib/release_manifest.py verify release-bundle --require-arch aarch64   # files_checked=14, ok
python3 deploy/lib/check_elf.py release-bundle/bin/khaen-rt-host --require-arch aarch64
python3 deploy/lib/check_elf.py release-bundle/bin/khaen-rt-daemon --require-arch aarch64
```

## 4. 安装/切换（授权窗口内，root；唯一入口 05）

```bash
sudo ./deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/window.json \
     --bundle /var/tmp/release-bundle --allow-first-switch
# 结果证据：/var/lib/khaen-rt-stack/evidence/<ts>-05-atomic-switch/{stages.log,result.json,...}
```

> 内部顺序：00 snapshot → 01 pre-install（失败=零变更）→ 04 install（release/current + units + env/toml + disable irqbalance/IRQ pin + keeper，不 start）
> → 01 完整 A–J → 03 retire-c（**F2：普通 unit 文件先备份移走再 mask**）→ 02 设备独占 → start keeper/irq-affinity
> → enable/start target（等 host+daemon active；**F3/F4 新 unit 生效**）→ 06 verify（V1–V10）→ 07 last-good。
> 失败只回自研 last-good 或 mute（G-no-C；**F5 的 mute 核对已改为运行时环境**）。

## 5. 板端遗留补救文件（release-v4 安装后可删）

release-v3 板窗的人工补救文件在新 unit 装载后不再需要（删除前确认新 unit 已生效）：
- `/etc/tmpfiles.d/khaen-rt-stack.conf`（F3 补救；现由 unit 的 RuntimeDirectory/LogsDirectory 提供）
- `/etc/systemd/system/khaen-rt-host.service.d/95-deviceallow.conf`（F4 补救；现 unit 内为 `DeviceAllow=char-alsa rw`）
- 手工 `/run/khaen-rt-stack`、`/var/log/khaen-rt-stack` 目录由 systemd 管理，不需保留。
保留名单与窗口动作见 `rt-stack/evidence/board-window-20261118/deploy/REPORT.md` §5.3。

## 6. 依赖与边界

- 板上需 `python3`、`systemctl`、`tar`、`sha256sum`；上传包不联网。
- **所有板端动作 NOT_RUN**：安装/systemd/真实 ALSA/MOTU 播放/RT/IRQ 持久化/浏览器/回滚演练均未执行
  （见 `rt-stack/evidence/release-v4/NOT_RUN.md`）。
- 禁止 CamillaDSP/旧 C unit 作任何回退；授权窗口 JSON 与 `audio_off/S1/soak` 口径见 `deploy/README.md` §1/§3。
- web dist 不在本包：板端若仍需替换，按 `web/scripts/install-device.sh` 单独上传
  （M4 重打 `d458ae7b…`，见 `rt-stack/evidence/release-v3/ARTIFACTS.md`）。
