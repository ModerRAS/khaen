# 音频栈 aarch64 构建执行记录（task-4，2026-11-15）

> 状态：进行中，构建完成后回填产物 sha256/耗时/日志路径。构建机=firmware-builder 的 VirtualBox Ubuntu 22.04 VM（8 vCPU/23 GB RAM/147 GB），SSH 127.0.0.1:2223。

## 1. 环境与方法

| 项 | 值 |
| --- | --- |
| 宿主 | Ubuntu 22.04.5 LTS，kernel 5.15.0-203-generic，x86_64 |
| 目标 | Debian 12 (bookworm) aarch64，基线 kernel 6.1.99（非 RT；RT 版另编） |
| chroot | debootstrap `--arch=arm64 --foreign --variant=minbase bookworm` + qemu-user-static/binfmt（qemu 6.2，binfmt F 模式）；apt 源 TUNA（宿主与 chroot 隔离，未污染宿主 apt） |
| Rust | 宿主 rustup 1.100.0 + `aarch64-unknown-linux-gnu` target；linker `aarch64-linux-gnu-gcc` + `--sysroot=<chroot>`；PKG_CONFIG_ALLOW_CROSS/SYSROOT_DIR 指向 chroot |
| C/C++ | 宿主 `aarch64-linux-gnu-g++` 11.4 + CMAKE_TOOLCHAIN_FILE（CMAKE_SYSROOT=chroot） |
| 离线包 | `fetch-debs.sh` 从 deb.debian.org bookworm arm64 抓 20 个运行时 .deb（MANIFEST.txt 含 sha256） |
| 目录 | `/home/builder/audio-build/{src,rootfs,release,logs,build}`；产物同步回 `projects/tspi-audio-dsp/audio/release/` |

## 2. 产物清单（待回填）

| 组件 | 方式 | 版本/ref | 产物 | sha256 |
| --- | --- | --- | --- | --- |
| CamillaDSP | 官方 aarch64 资产 | v4.1.3 | release/bin/camilladsp | 待填 |
| Inferno | 交叉 cargo | dev@9767558 | release/bin/inferno2pipe + release/lib/libasound_module_pcm_inferno.so | 待填 |
| Statime | 交叉 cargo | inferno-dev@244f20a | release/bin/statime | 待填 |
| AES67 daemon | 交叉 cmake | master@93314d2 + LKM aes67-daemon@8aea781 | release/bin/aes67-daemon（+ LKM 待内核） | 待填 |
| linuxptp | 官方 .deb 提取 | 3.1.1-4+b2 | release/bin/linuxptp/{ptp4l,phc2sys,pmc} | 待填 |
| 离线包池 | fetch-debs | bookworm arm64 | release/debs/*.deb + MANIFEST.txt | 见 MANIFEST |

## 3. 构建日志

- `logs/fetch-debs.log`（已生成）
- `logs/host-rustup.log`（已生成）
- `logs/stage2b-debootstrap.log`
- `logs/prep-sources.log`
- `logs/stage3-build.log`
- `logs/stage4-aes67.log`
- `logs/build-info.txt`、`release/MANIFEST.sha256`

## 4. 待办

- [ ] LKM：等 firmware-builder 的 6.1 kernel build dir（非 RT 先做编译烟雾测试；最终交付需 RT kernel build dir 重编）
- [ ] install-into-image.sh --dry-run（用同一 Debian12 rootfs）
- [ ] release 产物同步回 Windows 项目目录 + sha256 清单
- [ ] 若有任何交叉编译偏差，记录与 native 构建的差异

## 5. 备注

- 宿主 apt 未被污染（未 `dpkg --add-architecture arm64`）；arm64 依赖全部在 chroot 内安装。
- chroot 与宿主 apt/dpkg 锁隔离；构建期间未干扰 firmware-builder 的 SDK 构建。
- 构建产物在 chroot 内验证（`aarch64-linux-gnu-readelf` + qemu binfmt 直接执 `--version`）。
