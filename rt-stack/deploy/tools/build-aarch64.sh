#!/usr/bin/env bash
# T147/T149/G1 build-aarch64.sh —— 生成可部署的 aarch64 `khaen-rt-host` + `tspi-cal`（板端 Debian 12 / RK3576）
#
# 背景（T147）：Windows 开发机对 aarch64-unknown-linux-musl 交叉在 alsa-sys 处失败（缺 aarch64 libasound
#   pkg-config sysroot）。生产构建走 **aarch64-unknown-linux-gnu**（板端 glibc 2.36 + libasound2 1.2.8）。
# T149 已在项目 builder VM（Ubuntu 22.04 x86_64 + Debian12 aarch64 sysroot）用 --mode cross-sysroot 成功实做：
#   611,552 B / sha256 324683e8…dce9d / qemu selftest PASS（证据 evidence/deploy-artifacts/BUILD-GNU.md）。
#
# 模式：
#   native            在 aarch64 Debian 板上原生构建（需 rustc/cargo + libasound2-dev）
#   cross-sysroot     Linux x86_64 + 既有 aarch64 sysroot（本任务 builder VM 实际采用；需 --sysroot）
#   cross-multiarch   Linux x86_64 + dpkg multiarch（gcc-aarch64-linux-gnu + libasound2-dev:arm64）
#   cross-zig         Linux x86_64 + cargo-zigbuild（仍需 arm64 libasound sysroot）
#
# 默认 dry-run：只检查环境并打印命令；--live 才真正构建/打包。首次需可访问 crates.io 或本地 registry。
# 用法:
#   ./build-aarch64.sh --mode cross-sysroot --sysroot /home/builder/audio-build/rootfs \
#        --out /var/tmp/tspi-release-<id> --release-id <id> [--selftest-qemu PATH] [--with-daemon] [--live]
#   ./build-aarch64.sh --mode native --out /var/tmp/release [--live]
# 产物（G1 修复）：默认双 bin = khaen-rt-host + tspi-cal（goal-8 aarch64 迁移）；
#   --with-daemon 追加 tspi-rt-daemon（现行 service/ units 要求 host+daemon 同时 active 时使用）。
# 退出码: 0 成功；2 用法/缺输入；3 环境不满足（记录缺项，不假装构建）；7 构建/打包失败。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY="$(cd "$HERE/.." && pwd)"
RS="$(cd "$DEPLOY/.." && pwd)"
MODE=""; OUT=""; RELEASE_ID=""; LIVE=0; SYSROOT=""; QEMU=""; OFFLINE=0; WITH_DAEMON=0
TARGET="aarch64-unknown-linux-gnu"
while [ $# -gt 0 ]; do
  case "$1" in
    --mode) MODE="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --release-id) RELEASE_ID="$2"; shift 2 ;;
    --sysroot) SYSROOT="$2"; shift 2 ;;
    --selftest-qemu) QEMU="$2"; shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    --with-daemon) WITH_DAEMON=1; shift ;;
    --offline) OFFLINE=1; shift ;;
    --live) LIVE=1; shift ;;
    --dry-run) LIVE=0; shift ;;
    -h|--help) sed -n "2,32p" "$0" | sed "s/^# \{0,1\}//" >&2; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$MODE" ] || { echo "缺少 --mode native|cross-sysroot|cross-multiarch|cross-zig" >&2; exit 2; }
[ -n "$OUT" ] || { echo "缺少 --out（release bundle 输出目录）" >&2; exit 2; }
[ -n "$RELEASE_ID" ] || RELEASE_ID="local-$(date +%Y%m%dT%H%M%S)"
TARGET_DIR="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/tspi-rt-aarch64-target}"
export CARGO_TARGET_DIR="$TARGET_DIR"
say() { printf "[build] %s\n" "$*" >&2; }
warn() { printf "[build][warn] %s\n" "$*" >&2; }
die() { printf "[build][error] %s\n" "$*" >&2; exit "${2:-7}"; }
need_cmd_or_die() { command -v "$1" >/dev/null 2>&1 || die "缺少命令: $1（按 evidence/deploy-artifacts/TOOLCHAIN.md 安装）" 3; }
MISSING=0
need() { command -v "$1" >/dev/null 2>&1 && return 0; say "缺少命令: $1"; MISSING=$((MISSING+1)); return 1; }

say "mode=$MODE target=$TARGET out=$OUT target_dir=$TARGET_DIR live=$LIVE"
say "rustc: $(rustc -vV 2>/dev/null | sed -n "1,2p" | tr "\n" " " || echo MISSING)"
BUILD=()
VERIFY_ARGS=()
case "$MODE" in
  native)
    [ "$(uname -m)" = "aarch64" ] || die "native 模式需要在 aarch64 板/机器上执行（当前 $(uname -m)）" 3
    need_cmd_or_die cargo; need_cmd_or_die rustc; need_cmd_or_die pkg-config
    pkg-config --exists alsa || die "缺 libasound2-dev（pkg-config alsa 不存在）：apt install -y build-essential pkg-config libasound2-dev" 3
    BUILD=(cargo build --release --locked --target "$TARGET" -p khaen-rt-host --bin khaen-rt-host -p khaen-calibration --bin tspi-cal)
    ;;
  cross-sysroot)
    [ -n "$SYSROOT" ] || die "cross-sysroot 需要 --sysroot <Debian12 aarch64 rootfs>" 2
    [ -d "$SYSROOT" ] || die "sysroot 不存在: $SYSROOT" 3
    need_cmd_or_die cargo; need_cmd_or_die aarch64-linux-gnu-gcc
    [ -f "$SYSROOT/usr/lib/aarch64-linux-gnu/pkgconfig/alsa.pc" ] || die "sysroot 缺 alsa.pc（libasound2-dev:arm64/rootfs）: $SYSROOT" 3
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
    export PKG_CONFIG_PATH="$SYSROOT/usr/lib/aarch64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig"
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--sysroot=$SYSROOT"
    say "cross env: PKG_CONFIG_SYSROOT_DIR=$PKG_CONFIG_SYSROOT_DIR linker=aarch64-linux-gnu-gcc rustflags=$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS"
    BUILD=(cargo build --release --locked --target "$TARGET" -p khaen-rt-host --bin khaen-rt-host -p khaen-calibration --bin tspi-cal)
    VERIFY_ARGS=(--sysroot "$SYSROOT")
    ;;
  cross-multiarch)
    need_cmd_or_die cargo; need_cmd_or_die aarch64-linux-gnu-gcc
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig}"
    pkg-config --exists alsa || die "缺 arm64 libasound .pc（dpkg --add-architecture arm64; apt install libasound2-dev:arm64）" 3
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    BUILD=(cargo build --release --locked --target "$TARGET" -p khaen-rt-host --bin khaen-rt-host -p khaen-calibration --bin tspi-cal)
    ;;
  cross-zig)
    need_cmd_or_die cargo; need_cmd_or_die cargo-zigbuild
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig}"
    pkg-config --exists alsa || die "缺 arm64 libasound .pc；zig 只解决链接器，不提供 libasound sysroot" 3
    BUILD=(cargo zigbuild --release --locked --target "$TARGET" -p khaen-rt-host --bin khaen-rt-host -p khaen-calibration --bin tspi-cal)
    ;;
  *) die "未知 --mode: $MODE（native|cross-sysroot|cross-multiarch|cross-zig）" 2 ;;
esac
# G1（aarch64-20261122）：默认双 bin（host + cal）；--with-daemon 追加 daemon（现行 service/ 契约需要时）
[ "$WITH_DAEMON" = "1" ] && BUILD+=(-p khaen-control --bin tspi-rt-daemon)
[ "$OFFLINE" = "1" ] && BUILD+=("--offline")
BIN_DIR="$TARGET_DIR/$TARGET/release"
BIN="$BIN_DIR/khaen-rt-host"
BIN_CAL="$BIN_DIR/tspi-cal"
BIN_DAEMON="$BIN_DIR/tspi-rt-daemon"
[ "$MISSING" -eq 0 ] || die "环境缺项 $MISSING 个：只记录 NOT_BUILT，不假装成功" 3

if [ "$LIVE" = "0" ]; then
  say "[dry-run] 将执行: (cd $RS && ${BUILD[*]})"
  say "[dry-run] 将校验: $BIN + $BIN_CAL（check_elf/verify-artifact.sh；host 另跑 qemu selftest）+ release bundle"
  say "[dry-run] 将执行: bash $DEPLOY/bin/make-release-bundle.sh --live --target $TARGET --bin-dir $BIN_DIR --out $OUT --release-id $RELEASE_ID"
  exit 0
fi

cd "$RS" || die "无法进入 rt-stack: $RS" 7
say "构建：${BUILD[*]}"
"${BUILD[@]}" || die "cargo build 失败（交叉 sysroot/仓库/离线依赖；见 TOOLCHAIN.md）" 7
for b in "$BIN" "$BIN_CAL"; do
  [ -f "$b" ] || die "构建完成但找不到二进制: $b" 7
  say "产物: $(basename "$b") size=$(stat -c %s "$b") sha256=$(sha256sum "$b" | awk "{print \$1}")"
done
if [ "$WITH_DAEMON" = "1" ]; then
  [ -f "$BIN_DAEMON" ] || die "构建完成但找不到二进制: $BIN_DAEMON" 7
  say "产物: tspi-rt-daemon size=$(stat -c %s "$BIN_DAEMON") sha256=$(sha256sum "$BIN_DAEMON" | awk "{print \$1}")"
fi

# 执行/架构校验
if command -v qemu-aarch64-static >/dev/null 2>&1 && [ -n "$SYSROOT" ]; then
  QEMU="${QEMU:-$(command -v qemu-aarch64-static)}"
fi
if [ -n "$QEMU" ] && [ -n "$SYSROOT" ]; then
  say "qemu selftest: $QEMU -L $SYSROOT $BIN selftest"
  "$QEMU" -L "$SYSROOT" "$BIN" selftest || die "qemu selftest 失败（不得部署）" 7
else
  warn "无 qemu/sysroot：跳过 aarch64 selftest（记录 NOT_RUN；板端原生构建可直接执行）"
fi
ARCH_BINS=("$BIN" "$BIN_CAL"); [ "$WITH_DAEMON" = "1" ] && ARCH_BINS+=("$BIN_DAEMON")
if command -v python3 >/dev/null 2>&1; then
  for b in "${ARCH_BINS[@]}"; do python3 "$DEPLOY/lib/check_elf.py" "$b" --require-arch aarch64 || die "架构校验失败: $b" 7; done
fi
if [ -f "$DEPLOY/tools/verify-artifact.sh" ] && command -v aarch64-linux-gnu-readelf >/dev/null 2>&1; then
  VQ=(); [ -n "$QEMU" ] && [ -n "$SYSROOT" ] && VQ=(--qemu "$QEMU" --selftest)
  say "verify-artifact: $BIN ${VERIFY_ARGS[*]} ${VQ[*]}"
  bash "$DEPLOY/tools/verify-artifact.sh" "$BIN" "${VERIFY_ARGS[@]}" "${VQ[@]}" || die "verify-artifact 失败（不得部署）" 7
  # G6：cal/daemon 也过 ELF/依赖/GLIBC 门；两者无 selftest 子命令，不传 --selftest
  for b in "${ARCH_BINS[@]}"; do
    [ "$b" = "$BIN" ] && continue
    [ -f "$b" ] || continue
    say "verify-artifact: $b ${VERIFY_ARGS[*]}（无 selftest 子命令）"
    bash "$DEPLOY/tools/verify-artifact.sh" "$b" "${VERIFY_ARGS[@]}" || die "verify-artifact 失败: $b（不得部署）" 7
  done
else
  warn "verify-artifact.sh/readelf 不可用：跳过完整校验（记录 NOT_RUN）"
fi

bash "$DEPLOY/bin/make-release-bundle.sh" --live --target "$TARGET" --bin-dir "$BIN_DIR" \
  --out "$OUT" --release-id "$RELEASE_ID" || die "打包 bundle 失败" 7
python3 "$DEPLOY/lib/release_manifest.py" verify "$OUT" --require-arch aarch64 || die "bundle manifest 校验失败" 7
say "完成：$OUT"
say "下一步（T71 授权窗口）：05-atomic-switch.sh --live --authorize <window> --bundle $OUT"
exit 0
