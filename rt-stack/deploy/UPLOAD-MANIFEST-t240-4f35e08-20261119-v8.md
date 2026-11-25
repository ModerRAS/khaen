# UPLOAD-MANIFEST — 板端上传清单（release-v8 / @RELEASE_ID@，task-240）

> **现行板端上传依据（2026-11-19 release-v8：AES67 userspace host 接线 + 当前 master 重打 aarch64）**；取代
> release-v7（t226-961f306-20261118-v7）。板端动作全部 **NOT_RUN**，必须在授权窗口内、按 deploy/README.md 流程执行。
> 构建/重打包证据（逐文件 sha、qemu、解包校验）：rt-stack/evidence/release-v8/。
>
> 上传包：rt-stack/evidence/release-v8/artifact/tspi-deploy-@RELEASE_ID@.tgz。本清单在包内，包自身 sha256 记在包外
> （artifact/*.sha256 与 ARTIFACTS.md），避免自引用；校验以包外 sha256 + 包内 SHA256SUMS（sha256sum -c）+ 板端 test -x 为准。
>
> 构建源：**clean git HEAD 4f35e08**（2026-11-19T01:15:06+08:00；T239 AES67 userspace host 接线 + T235 aoip-aes67 crate +
> T226 REST CalibrationPort + ca2b4a3 pacing（v6）+ fe545a2 T72-N9 unit 修复（v6b）+ 当前 deploy/service）。bundle release_id = @RELEASE_ID@。

## 1. 上传包 → 板端目标（由板上脚本安装，不手工拷）

| 包内目录/文件 | 内容 | 板端去向（04/05 安装） |
| --- | --- | --- |
| deploy/ | bin/00..08 + make-release-bundle.sh + make-deploy-package.sh、lib/、config/、tools/、tests/、README、本 UPLOAD-MANIFEST | /var/tmp/tspi-rt-stack-deploy/（root 执行） |
| service/ | host/daemon/target/irq-affinity units + host.service.d/90-safe-mute.{conf,env}.example + README | /etc/systemd/system/（04 安装；target Wants/After host+daemon） |
| release-bundle/ | bin/khaen-rt-host（v8，含 AES67 userspace）、bin/tspi-rt-daemon（v8，含 T226）、units/（6）、etc/（5 个 *.example）、manifest.json、README.deploy.md、SHA256SUMS | /opt/tspi-rt-stack/releases/@RELEASE_ID@/{bin,units,manifest.json,README.deploy.md} + current（symlink） |
| README-UPLOAD.md、SHA256SUMS | 上传说明 + 包级逐文件校验和 | /var/tmp/ 保留（校验用） |

## 2. 关键文件 sha256（完整清单见包内 SHA256SUMS；bundle 14 项见 release-v8/ARTIFACTS.md）

| 文件 | 大小(B) | sha256 | 说明 |
| --- | ---: | --- | --- |
| bin/khaen-rt-host（bundle，v8 重编译） | @HOST_SIZE@ | @HOST_SHA@ | **新增 AES67 userspace transport**（SDP→组播/单播→128f→H16→图）+ v7 全部功能 |
| bin/tspi-rt-daemon（bundle，v8 重编译） | @DAEMON_SIZE@ | @DAEMON_SHA@ | 含 T226 CalibrationPort（源码未变，预期与 v7 字节相同） |
| bundle tgz（可单独上传） | @BUNDLE_TGZ_SIZE@ | @BUNDLE_TGZ_SHA@ | 14 files；manifest verify ok；bin 0755 |
| web dist（独立服务，不在本包） | @WEB_SIZE@ | @WEB_SHA@ | 源码自 v5 未变，重打预期字节相同 |

> 与 release-v7 比：**host 变化**（含 task-235/239 AES67 用户态接收端与 userspace 传输分支；v7 host 无此代码）；
> **daemon/web 预期不变**（源码未变）；units/etc/deploy 全部保持 v7（v6b T72-N9 + F1–F5 不变）。
> 逐项对比见 release-v8/DIFF-vs-v7.md。

## 3. 上传与板端只读校验

    scp tspi-deploy-@RELEASE_ID@.tgz board:/var/tmp/
    ssh board 'cd /var/tmp && rm -rf tspi-deploy-v8 && mkdir tspi-deploy-v8 && tar xzf tspi-deploy-@RELEASE_ID@.tgz -C tspi-deploy-v8'
    cd /var/tmp/tspi-deploy-v8 && sha256sum -c SHA256SUMS
    test -x deploy/bin/05-atomic-switch.sh && test -x deploy/bin/03-retire-c.sh && echo SCRIPT_EXEC_OK
    python3 deploy/lib/release_manifest.py verify release-bundle --require-arch aarch64   # files_checked=14, ok
    python3 deploy/lib/check_elf.py release-bundle/bin/khaen-rt-host --require-arch aarch64
    python3 deploy/tests/check_unit_syntax.py deploy/config service       # 7/7 PASS
    grep -a -q 't60-jobmanager-synthetic' release-bundle/bin/tspi-rt-daemon && echo DAEMON_T226_MARKER_OK
    grep -a -q 'aes67_transport' release-bundle/bin/khaen-rt-host && grep -a -q 'khaen-rt-host/aes67-rx' release-bundle/bin/khaen-rt-host && echo HOST_AES67_MARKER_OK
    release-bundle/bin/khaen-rt-host run --help | grep -q 'aes67_transport=userspace' && echo HOST_AES67_HELP_OK

## 4. 安装/切换（授权窗口内，root；唯一入口 05）

    systemctl stop tspi-irq-affinity.service   # 已装板复跑前停残留 oneshot（F-v5-1）
    sudo ./deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/window.json \
         --bundle /var/tmp/tspi-deploy-v8/release-bundle --allow-first-switch

内部顺序：00 snapshot → 01 pre-install → 04 install（release/current + units + env/toml + keeper，不 start）→ 01 完整 A–J
→ 03 retire-c → 02 设备独占 → start keeper/irq-affinity → enable/start target → 06 verify（V1–V10）→ 07 last-good。
失败只回自研 last-good 或 mute（G-no-C）。

## 5. v8 板窗必验（AES67 静音链路为新增核心）

前置：MOTU/功放物理静音（CueMix 不改）；窗口内不启停 systemd；PC 与板同网段、PC→板组播路径可达。
详细步骤与判据：rt-stack/evidence/release-v8/BOARD-AES67-SILENT-TEST.md（配套 task-239 脚本在
rt-stack/evidence/aes67-host-wire/board/，不在本上传包内，需单独放行/上传）。

1. 板端只读：`release-bundle/bin/khaen-rt-host run --help` 含 `aes67_transport=userspace` 与 `aes67_sdp`。
2. 静音链路（默认 backend=fake，不开任何音频设备）：PC 起 `rt-stack/tools/aes67-sender/target/release/aes67-send.exe`
   （sine 1k/2k → 239.69.63.10:5004，L24/48k/1ms）→ 板端 `khaen-rt-host run --config host.toml --blocks 3000`
   → 抓 `host.json`。判据：`status=ok`、`capture.source=aes67`、`transport=userspace`、`data_blocks>=1`、
   `accepted_packets>=1`、`loss_events=0`；板端脚本若检测到已有 `khaen-rt-host run` 会 REFUSE（不杀进程/不碰服务）。
3. 可选：`backend=alsa`（host.alsa-muted.toml，-MuteConfirmed）验证真实播放链；任何出声前必须完成外部静音检查。
4. 其余 v7 回归项保持：daemon ready 行 T226 字段、/var/lib/tspi-rt-stack/calibration/、REST synthetic 闭环（仅 REST 闭环，
   不当 T67/T68 证据）；web 静态资源 hash（G4 已随 v7 解除，本包不变）。
5. 结束还原：PC sender 停、MOTU kMainTrim 22、CueMix 直通路由不动、-50 dB 起步、不做 192k；证据留在 /tmp 目录供取证。

## 6. 依赖与边界

- 板上需 python3、systemctl、tar、sha256sum；上传包不联网。
- 所有板端动作 **NOT_RUN**（安装/systemd/真实 ALSA/MOTU 播放/RT/IRQ 持久化/浏览器/AES67 真实网络与 PTP/回滚演练）。
- AES67 userspace 时钟为进程内 UsrvClock，**非 PTP**（报告显式 ptp=not-wired）；板端 PTP/PHC 对齐另行验证。
- 禁 CamillaDSP/旧 C unit 作任何回退；授权窗口 JSON 与 audio_off/S1/soak 口径见 deploy/README.md §1/§3。
