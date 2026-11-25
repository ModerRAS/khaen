# aarch64 构建信息（release aarch64-20261122）

> goal-8「aarch64 交叉编译生产双 bin + 可部署 bundle」在 builder VM 上**离线**构建。
> 证据：`rt-stack/evidence/calib-chain-20261121/aarch64-build-20261122/`（构建日志、readelf、qemu 自检、manifest、SHA256SUMS）。

## 0. 一句话

在 VirtualBox builder VM `tspi-audio-build`（Ubuntu 22.04 x86_64 + Debian 12 arm64 sysroot）上，用
`deploy/tools/build-aarch64.sh --mode cross-sysroot --with-daemon` 从 **clean HEAD 源**（`git archive HEAD`，
commit `5c8ea11c93499bb36f33520a548eddfbd47d2da1`）产出 aarch64 `tspi-rt-host` + `tspi-cal`（+ 附带 `tspi-rt-daemon`），
再经 `make-release-bundle.sh` 打成 release bundle。全程离线（`cargo --offline`）。
**板端（泰山派 3M，192.0.2.215）安装/运行 = NOT_RUN**：板关机且不可达，本任务未触达。

## 1. 构建链（可复现）

| 项 | 值 |
| --- | --- |
| 源 | `git archive HEAD` → `rt-stack/{Cargo.toml,Cargo.lock,crates,tools/aes67-rx,tools/mimo-opt,deploy,service}`；HEAD `5c8ea11` |
| 源包 | `rt-src.tar` 12,042,240 B；sha256 `54edb42219dc44b6c1006ed83a4b4cba69da2b5f054837532fc67bb0c8646e9b` |
| 构建机 | VirtualBox VM `tspi-audio-build`：Ubuntu 22.04.5 x86_64，8 vCPU / 24 GB RAM，SSH 127.0.0.1:2223 |
| Rust | rustc/cargo **1.100.0**（VM 内 rustup，target `aarch64-unknown-linux-gnu` 已装） |
| C 交叉 | `aarch64-linux-gnu-gcc` **11.4.0** / binutils-aarch64-linux-gnu |
| sysroot | `/home/builder/audio-build/rootfs` = Debian 12 bookworm arm64：glibc **2.36**、libasound2 **1.2.8**（含 `alsa.pc`） |
| qemu | `qemu-aarch64-static` 6.2.0（runtime 自检；不碰真声卡） |
| 依赖 | 离线 registry 缓存命中（372 crate；`cargo fetch --locked --offline` exit 0） |

交叉环境变量（脚本已内置）：

```
PKG_CONFIG_ALLOW_CROSS=1
PKG_CONFIG_SYSROOT_DIR=<rootfs>
PKG_CONFIG_PATH=<rootfs>/usr/lib/aarch64-linux-gnu/pkgconfig:<rootfs>/usr/share/pkgconfig
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-C link-arg=--sysroot=<rootfs>
```

执行命令：

```bash
bash deploy/tools/build-aarch64.sh --mode cross-sysroot --sysroot /home/builder/audio-build/rootfs \
  --out <bundle> --release-id aarch64-20261122 --with-daemon \
  --selftest-qemu "$(command -v qemu-aarch64-static)" --live
```

脚本内部核心构建（G1 修复后）：

```bash
cargo build --release --locked --offline --target aarch64-unknown-linux-gnu \
  -p tspi-rt-host --bin tspi-rt-host \
  -p tspi-calibration --bin tspi-cal \
  -p tspi-control --bin tspi-rt-daemon        # 仅 --with-daemon 时
```

构建耗时：三 bin 合计 **24.7 s**（8 核；`/usr/bin/time -v` user 112.7 s / sys 33.7 s / wall 24.7 s，exit 0）。

## 2. 产物清单与 SHA256

| bin | size (B) | sha256 | 角色 |
| --- | ---: | --- | --- |
| `bin/tspi-rt-host` | 1,451,984 | `40f158669da28ee94c95a6fbeb78de06004c4d60e8a43c3fb76ef24584fa8f9b` | **目标双 bin 之一**：RT DSP host（生产服务） |
| `bin/tspi-cal` | 2,608,352 | `7cc598d2616df6ea1c81e8522b5eebba247de3226971803a791c9251ad0c047b` | **目标双 bin 之一**：校准链 CLI（T67 前置；离线 fake 引擎） |
| `bin/tspi-rt-daemon` | 3,171,080 | `6db828d15d9ffcb07ad602d4b42642fc8ce0dc6318548939c620e0a73eeaf6fe` | **附带**：control-plane daemon（现行 units/05 切换契约要求 host+daemon 同 active，随包保证 bundle 自一致） |

三 bin 共同 ELF 属性（`file`/`readelf` 全量见证据 `elf/*.txt`）：

- `ELF 64-bit LSB pie executable, ARM aarch64`，Type=`DYN`（PIE），interpreter `/lib/ld-linux-aarch64.so.1`
- NEEDED：`libasound.so.2`、`libgcc_s.so.1`、`libc.so.6`、`ld-linux-aarch64.so.1`
- **动态 PIE（非静态）**：板端是 glibc 生态 + 动态 libasound；musl 静态路径不适用（TOOLCHAIN.md R9）
- GLIBC 符号版本 max = **2.34** ≤ 板端 bookworm **2.36**（verify-artifact.sh PASS）
- loader `--list`（等价 ldd，`qemu -L <sysroot> <rootfs>/lib/ld-linux-aarch64.so.1 --list <bin>`）全部依赖可解析

bundle tgz：`dist/tspi-aarch64-20261122-deploy.tgz`，**3,017,527 B**，
sha256 `c3218eb2b69ca84c18c3da09375e9ecfc8b1c24897f76e5fd7e13be8126fa6e5`
（19 成员；`bin/*` 全部 0755；`release_tar.py verify` sums_checked=18 PASS）。

## 3. 质量门（全部 PASS，均在 VM 内离线执行）

| 门 | 结果 |
| --- | --- |
| 编译/链接 | 3 bin exit 0（`cargo build --release --locked --offline`） |
| `check_elf.py --require-arch aarch64` ×3 | PASS |
| `verify-artifact.sh` ELF/NEEDED/GLIBC/sysroot 门 ×3 | PASS；host 另含 qemu `--selftest` PASS |
| qemu `tspi-rt-host selftest` | PASS：status=ok，workers=4，bit_exact_graphs=2，format_nonfinite=0 |
| qemu `tspi-rt-host admission-check` | PASS：status=ok，admission=plan_memory-ok，scheduler=t45-plan-apply-wired |
| qemu `tspi-rt-host fake-io --blocks 8` | PASS：spsc 8/8，format_nonfinite=0 |
| qemu `tspi-rt-daemon check --config`（bind 127.0.0.1:18080） | PASS：status=ok，warnings=[] |
| qemu `tspi-cal run --design synthetic`（fake 引擎、离线） | exit 0；job-0001 走到 compile（plan.bin 716 B）后按 dry-run 语义 rolled_back；board_run=NOT_RUN |
| units 契约（B1 `ExecStartPre=irq-affinity-pin wait`） | PASS（check_unit_syntax 6/6 + 打包时 B1 grep 门） |
| `release_manifest.py verify --require-arch aarch64` | PASS：files_checked=17，binaries=[host,daemon,cal] |
| tgz 成员模式/清单 | PASS（bin/*=0755；sums 18） |

CLI 事实（不作假）：三 bin **都没有 `--version` 子命令**——host 对 `--version` 回 `unknown command` exit 2；
cal 对 `--version` 回 `missing value for --version`；daemon 无 `--version`，无参回 usage exit 2。
`--version` 记 **NOT_SUPPORTED**，自检以 `selftest`/`admission-check`/`fake-io`/`check`/`run --design synthetic` 为准。

## 4. 脚本缺口修复（G1 + G5/G6）

1. **G1（高）**：`deploy/tools/build-aarch64.sh` 原先 4 个 mode 都只 `cargo build -p tspi-rt-host`。
   现**默认构建双 bin**（`tspi-rt-host` + `tspi-cal`），新增 `--with-daemon` 追加 `tspi-rt-daemon`；
   构建后对每个 bin 跑 `check_elf`/`verify-artifact`（G6：cal/daemon 也过架构门；只有 host 跑 qemu selftest）。
2. **G5（中）**：`deploy/bin/make-release-bundle.sh` 原先只收 host+daemon；现在 `tspi-cal` 存在时自动入包
   （manifest kind=binary → `release_manifest.py verify --require-arch` 会一并校验三 bin 架构）。

## 5. 诚实边界（NOT_RUN）

- 板端（泰山派 3M / RK3576，192.0.2.215）**安装/启动/重启持久性/真实 ALSA/MOTU/AES67/PTP/RT/IRQ/Web = NOT_RUN**
  ——板关机且不可达，本任务未触达、未连接、未发任何板端命令；
- 真实声卡出声 = NOT_RUN（全程 fake/no-device；VM 无直通声卡）；
- `tspi-cal` 真实测量引擎（`--engine tools`/ALSA arecord/aplay）与真 apply = NOT_RUN（需板端窗口 + 授权）；
- daemon `serve`（常驻绑定端口）未跑，只跑 `check`（bind 校验，127.0.0.1:18080 非特权端口）；
- 二进制 **不入 git**（repo-hygiene：docs/REPO-HYGIENE-PLAN.md）。bin/ 与 dist/*.tgz 在本机磁盘交付，
  并以 `artifacts/audio/tspi-aarch64-20261122/` 同份留档；git 只提交 manifest/SHA256SUMS/units/etc/docs。

## 6. 复现（builder VM，离线）

```bash
# 1) 干净源（Windows 侧）
git -C <repo> archive --format=tar -o rt-src.tar HEAD rt-stack/Cargo.toml rt-stack/Cargo.lock \
  rt-stack/crates rt-stack/tools/aes67-rx rt-stack/tools/mimo-opt rt-stack/deploy rt-stack/service
# 2) 传 VM 并解包到 ~/aarch64-build-20261122/src（scp -P 2223 -i <key> ... builder@127.0.0.1）
# 3) 构建 + 门 + bundle
export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR=$HOME/aarch64-build-20261122/target
bash deploy/tools/build-aarch64.sh --mode cross-sysroot --sysroot /home/builder/audio-build/rootfs \
  --out $HOME/aarch64-build-20261122/bundle --release-id aarch64-20261122 --with-daemon \
  --selftest-qemu "$(command -v qemu-aarch64-static)" --live
```

详见证据目录 `REPORT.md`（状态矩阵 / 风险 / NOT_RUN 清单）与 `PROVENANCE.md`（源与工具链取证）。
