# UPLOAD-MANIFEST — 板端上传清单（release-v9 / @RELEASE_ID@，task-244）

> **现行板端上传依据（2026-11-19 release-v9：planrt 实时修复 + [sink]/[capture] tee + dsp_cpus 4-7 + 当前 master 重打 aarch64）**；
> 取代 release-v8（t240-4f35e08-20261119-v8）。板端动作全部 **NOT_RUN**，必须在授权窗口内、按 deploy/README.md 流程执行。
> 构建/重打包证据（逐文件 sha、qemu、解包校验）：rt-stack/evidence/release-v9/。
>
> 上传包：rt-stack/evidence/release-v9/artifact/tspi-deploy-@RELEASE_ID@.tgz。本清单在包内，包自身 sha256 记在包外
> （artifact/*.sha256 与 ARTIFACTS.md），避免自引用；校验以包外 sha256 + 包内 SHA256SUMS（sha256sum -c）+ 板端 test -x 为准。
>
> 构建源：**clean git HEAD 0f90a49**（2026-11-19T12:17:31+08:00；dsp_cpus 默认 4-7；含 1e1f77c planrt RT 修复
> + [sink] 离线渲染 + [capture] tee，4f35e08 AES67 userspace，a115d59 T226，现行 deploy/service）。
> bundle release_id = @RELEASE_ID@。**14:30 MIMO pilot 路线 A 的前置包**。

## 1. 上传包 → 板端目标（由板上脚本安装，不手工拷）

| 包内目录/文件 | 内容 | 板端去向（04/05 安装） |
| --- | --- | --- |
| deploy/ | bin/00..08 + make-release-bundle.sh + make-deploy-package.sh、lib/、config/、tools/、tests/、README、本 UPLOAD-MANIFEST | /var/tmp/tspi-rt-stack-deploy/（root 执行） |
| service/ | host/daemon/target/irq-affinity units + host.service.d/90-safe-mute.{conf,env}.example + README | /etc/systemd/system/（04 安装；target Wants/After host+daemon） |
| release-bundle/ | bin/tspi-rt-host（v9，planrt+sink/tee+AES67）、bin/tspi-rt-daemon（v9，T226）、units/（6）、etc/（5 个 *.example）、manifest.json、README.deploy.md、SHA256SUMS | /opt/tspi-rt-stack/releases/@RELEASE_ID@/{bin,units,manifest.json,README.deploy.md} + current（symlink） |
| README-UPLOAD.md、SHA256SUMS | 上传说明 + 包级逐文件校验和 | /var/tmp/ 保留（校验用） |

## 2. 关键文件 sha256（完整清单见包内 SHA256SUMS；bundle 14 项见 release-v9/ARTIFACTS.md）

| 文件 | 大小(B) | sha256 | 说明 |
| --- | ---: | --- | --- |
| bin/tspi-rt-host（bundle，v9 重编译） | @HOST_SIZE@ | @HOST_SHA@ | **新增 planrt RT 修复 + [sink] 离线渲染 + [capture] tee；dsp_cpus 默认 4-7** + v8 全部功能 |
| bin/tspi-rt-daemon（bundle，v9 重编译） | @DAEMON_SIZE@ | @DAEMON_SHA@ | 含 T226 CalibrationPort（control 源码未变，链接图随 rt-host lib 变化） |
| bundle tgz（可单独上传） | @BUNDLE_TGZ_SIZE@ | @BUNDLE_TGZ_SHA@ | 14 files；manifest verify ok；bin 0755 |
| web dist（独立服务，不在本包） | @WEB_SIZE@ | @WEB_SHA@ | 源码自 v5 未变，重打预期字节相同 |

> 与 release-v8 比：**host 变化**（planrt 实时修复、sink/tee、dsp 默认 4-7）；**daemon 重编译**（链接图变，行为不变）；
> **web 预期不变**；units 不变，etc 模板 rt-host.toml/env 的 dsp_cpus 改 4-7。逐项对比见 release-v9/DIFF-vs-v8.md。

## 3. 上传与板端只读校验

    scp tspi-deploy-@RELEASE_ID@.tgz board:/var/tmp/
    ssh board 'cd /var/tmp && rm -rf tspi-deploy-v9 && mkdir tspi-deploy-v9 && tar xzf tspi-deploy-@RELEASE_ID@.tgz -C tspi-deploy-v9'
    cd /var/tmp/tspi-deploy-v9 && sha256sum -c SHA256SUMS
    test -x deploy/bin/05-atomic-switch.sh && test -x deploy/bin/03-retire-c.sh && echo SCRIPT_EXEC_OK
    python3 deploy/lib/release_manifest.py verify release-bundle --require-arch aarch64   # files_checked=14, ok
    python3 deploy/lib/check_elf.py release-bundle/bin/tspi-rt-host --require-arch aarch64
    python3 deploy/tests/check_unit_syntax.py deploy/config service       # 7/7 PASS
    grep -a -q 't60-jobmanager-synthetic' release-bundle/bin/tspi-rt-daemon && echo DAEMON_T226_MARKER_OK
    grep -a -q '[sink] out' release-bundle/bin/tspi-rt-host && grep -a -q '[capture] tee' release-bundle/bin/tspi-rt-host && echo HOST_PLANRT_MARKER_OK
    release-bundle/bin/tspi-rt-host run --help | grep -q 'aes67_transport=userspace' && echo HOST_AES67_HELP_OK

## 4. 安装/切换（授权窗口内，root；唯一入口 05）

    systemctl stop tspi-irq-affinity.service   # 已装板复跑前停残留 oneshot（F-v5-1）
    sudo ./deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/window.json \
         --bundle /var/tmp/tspi-deploy-v9/release-bundle --allow-first-switch

内部顺序：00 snapshot → 01 pre-install → 04 install（release/current + units + env/toml + keeper，不 start）→ 01 完整 A–J
→ 03 retire-c → 02 设备独占 → start keeper/irq-affinity → enable/start target → 06 verify（V1–V10）→ 07 last-good。
失败只回自研 last-good 或 mute（G-no-C）。

## 5. v9 板窗必验（先 mute healthz/verify，再 14:30 路线 A）

前置：MOTU/功放物理静音（CueMix 不改）；窗口内不启停 systemd 之外的进程；48k（禁 176.4k/192k）。
完整步骤/判据：rt-stack/evidence/release-v9/ROUTE-A-14H30.md（配套 14h30 kit 在板端 /var/tmp/tspi-planrt/14h30/ 已就位）。

1. **安装后 mute 验收**：host active + SAFE_MODE=mute、healthz=200、06 verify V1–V10 PASS；host 运行报告
   scheduler.dsp_cpus=[4,5,6,7]；IRQ 58/56/57 → 1/2/3（均 A53，irqbalance inactive）。
   **注意**：板端已装 /etc/tspi-rt-stack/rt-host.env 仍为 TSPI_HOST_DSP_CPUS=2-5（04 不覆盖），
   窗口内改为 4-7 或删除该行后重启 host，再复核（预算 1–2 min；改前记录原值，失败回滚）。
2. **路线 A（推荐，10–15 min）**：离线渲染两臂（host run + [sink] out；直接用 current 二进制，停/不动服务均可，
   脚本检测到运行中的 host run 会 REFUSE）→ 同路径 aplay/arecord → PC 分析 → 证据回写。
3. 可选 B 路线：G-2560 真·实时 + [capture] tee（降支撑 pilot，需标注 L_G=2560 不等价）。
4. 结束还原：host/daemon/keeper active、SAFE_MODE=mute、healthz 200；CueMix 0 写；/dev/shm 清理。
5. 其余 v8 回归项保持：daemon ready 行 T226 字段、/var/lib/tspi-rt-stack/calibration/、web 静态资源 hash。

## 6. 依赖与边界

- 板上需 python3、systemctl、tar、sha256sum；上传包不联网。
- 所有板端动作 **NOT_RUN**（安装/systemd/真实 ALSA/MOTU 播放/RT/IRQ 持久化/浏览器/AES67 真实网络与 PTP/回滚演练）。
- AES67 userspace 时钟为进程内 UsrvClock，**非 PTP**（报告显式 ptp=not-wired）；板端 PTP/PHC 对齐另行验证。
- G-4096 真实时仍不达标（314.8 < 375 blocks/s）：路线 A 用离线渲染，**不声称实时**；B 路线 G-2560 为降支撑 pilot。
- 禁 CamillaDSP/旧 C unit 作任何回退；授权窗口 JSON 与 audio_off/S1/soak 口径见 deploy/README.md §1/§3。
