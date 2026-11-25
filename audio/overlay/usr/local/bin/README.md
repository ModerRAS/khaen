# overlay/usr/local/bin/README.md —— 二进制由 build-*.sh 产出后由 install-into-image.sh 注入

本目录在源码树里**不放二进制**（避免仓库膨胀/架构混淆）。期望产物（aarch64）来自：

| 文件 | 来源脚本 | 目标路径 | 说明 |
| --- | --- | --- | --- |
| camilladsp | build-camilladsp.sh | /usr/local/bin/camilladsp | CamillaDSP v4.1.3 |
| statime | build-inferno.sh | /usr/local/bin/statime | PTPv1 follower（teodly fork） |
| inferno2pipe | build-inferno.sh | /usr/local/bin/inferno2pipe | 可选录音工具 |
| aes67-daemon | build-aes67-daemon.sh | /usr/local/bin/aes67-daemon | AES67 daemon |
| libasound_module_pcm_inferno.so | build-inferno.sh | /usr/lib/aarch64-linux-gnu/alsa-lib/ | Inferno ALSA 插件 |
| MergingRavennaALSA.ko | build-aes67-daemon.sh | /lib/modules/$(uname -r)/extra/ | AES67 LKM |
| ptp4l/phc2sys/pmc | build-linuxptp.sh（或 .deb） | /usr/sbin/ | linuxptp 3.1.1 |

`tspi-mode`、`tspi-ptp-status`、`ptp-status-hook`、UCM 回移脚本是本目录自带的 shell 脚本（已随 overlay 提供）。
