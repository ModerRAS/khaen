#!/usr/bin/env bash
# build-aes67-daemon.sh — AES67 Linux daemon + MergingRavennaALSA 内核模块（ARM64）
#
# 固定版本（2026-11-15 核对）：
#   aes67-linux-daemon  master 93314d2b8295256089bdd847c6cca3dcbc9ec23e（2026-10-25）
#   ravenna-alsa-lkm    分支 aes67-daemon 8aea7813ddef4ab20ad7afc9e1308b3a10395703（driver v2.4）
#
# 产物：
#   release/bin/aes67-daemon
#   release/modules/MergingRavennaALSA.ko      （必须用目标内核 6.1.x(+RT) 的 build 目录编译）
#
# 用法：./build-aes67-daemon.sh [--kernel-dir /lib/modules/$(uname -r)/build] [--jobs N] [--mock] [--offline]
#   --mock  只编 daemon（FAKE_DRIVER=ON，不编 LKM），用于 x86/无内核头的开发机验证 REST/配置；不产出可用音频。
# 依赖：cmake/g++/make、boost、avahi、libsystemd-dev（见 packages.list [build]）；LKM 需要目标 kernel 头/Kbuild。
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/build-common.sh"

DAEMON_REF="93314d2b8295256089bdd847c6cca3dcbc9ec23e"
LKM_REF="8aea7813ddef4ab20ad7afc9e1308b3a10395703"
KERNEL_DIR="/lib/modules/$(uname -r)/build"
JOBS="$(nproc 2>/dev/null || echo 2)"
MOCK=0
OFFLINE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --kernel-dir) KERNEL_DIR="$2"; shift 2 ;;
    --jobs) JOBS="$2"; shift 2 ;;
    --mock) MOCK=1; shift ;;
    --offline) OFFLINE=1; shift ;;
    -h|--help) sed -n "2,15p" "$0"; exit 0 ;;
    *) die "unknown option: $1" 2 ;;
  esac
done

need_cmd cmake; need_cmd g++; need_cmd make
SRCD="$SRC_DIR/aes67-linux-daemon"
if [[ ! -d "$SRCD/.git" ]]; then
  [[ "$OFFLINE" == 1 ]] && die "offline 模式需要预置源码: $SRCD" 3
  need_cmd git
  git clone --depth 1 --no-checkout https://github.com/bondagit/aes67-linux-daemon.git "$SRCD"
  git -C "$SRCD" fetch --depth 1 origin "$DAEMON_REF" && git -C "$SRCD" checkout -f FETCH_HEAD \
    || { warn "按 sha 拉取失败，改用 master"; git -C "$SRCD" fetch --depth 1 origin master && git -C "$SRCD" checkout -f FETCH_HEAD; }
fi
log "daemon HEAD: $(git -C "$SRCD" rev-parse HEAD)"
if [[ "$OFFLINE" != 1 ]]; then
  git -C "$SRCD" submodule update --init --recursive --depth 1 || die "子模块初始化失败"
else
  [[ -d "$SRCD/3rdparty/cpp-httplib" && -d "$SRCD/3rdparty/ravenna-alsa-lkm" ]] \
    || die "offline: 子模块目录缺失（3rdparty/cpp-httplib、3rdparty/ravenna-alsa-lkm）" 3
fi
LKMD="$SRCD/3rdparty/ravenna-alsa-lkm"
if [[ -d "$LKMD/.git" ]]; then
  if [[ "$OFFLINE" != 1 ]]; then
    git -C "$LKMD" fetch --depth 1 origin "$LKM_REF" >&2 || true
  fi
  git -C "$LKMD" checkout -f "$LKM_REF" >&2 || warn "LKM 未能切到 $LKM_REF（沿用子模块默认）"
fi
log "LKM HEAD: $(git -C "$LKMD" rev-parse HEAD 2>/dev/null || echo unknown)"
record "aes67-daemon: ref=$DAEMON_REF lkm_ref=$LKM_REF kernel_dir=$KERNEL_DIR mock=$MOCK"

BUILDD="$REL_DIR/build/aes67-daemon"
rm -rf "$BUILDD"; mkdir -p "$BUILDD" "$MOD_DIR"

if [[ "$MOCK" == 0 ]]; then
  [[ -f "$KERNEL_DIR/Makefile" ]] || die "内核构建目录无效: $KERNEL_DIR（用 --kernel-dir 指向 6.1.x(+RT) 的 kernel build 目录）" 3
  log "编 LKM: make -C $KERNEL_DIR M=$LKMD/driver modules"
  make -C "$KERNEL_DIR" M="$LKMD/driver" modules -j"$JOBS"
  KO="$(ls -1 "$LKMD/driver/MergingRavennaALSA.ko" 2>/dev/null || true)"
  [[ -n "$KO" ]] || die "LKM 编译未产出 MergingRavennaALSA.ko" 4
  install_artifact "$KO" "$MOD_DIR/MergingRavennaALSA.ko" 644
  check_elf_aarch64 "$MOD_DIR/MergingRavennaALSA.ko"
else
  warn "--mock: 跳过 LKM 编译（仅用于开发机验证 daemon）"
fi

log "编 daemon（FAKE_DRIVER=$([[ "$MOCK" == 1 ]] && echo ON || echo OFF)）"
cmake -S "$SRCD/daemon" -B "$BUILDD" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCPP_HTTPLIB_DIR="$SRCD/3rdparty/cpp-httplib" \
  -DRAVENNA_ALSA_LKM_DIR="$LKMD" \
  -DWITH_AVAHI=ON \
  -DWITH_SYSTEMD=ON \
  -DWITH_STREAMER=OFF \
  -DWITH_NMOS=OFF \
  -DENABLE_TESTS=OFF \
  -DFAKE_DRIVER=$([[ "$MOCK" == 1 ]] && echo ON || echo OFF)
cmake --build "$BUILDD" -j"$JOBS"
install_artifact "$BUILDD/aes67-daemon" "$BIN_DIR/aes67-daemon" 755
check_elf_aarch64 "$BIN_DIR/aes67-daemon"
record "aes67-daemon: artifacts aes67-daemon$( [[ "$MOCK" == 0 ]] && echo "+MergingRavennaALSA.ko" || echo "" )"
print_summary
