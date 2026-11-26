#!/usr/bin/env bash
# khaen 发布构建：按目标架构给通用 CPU 调优（不做微架构特调），支持直接交叉编译。
#
# 用法：
#   ./deploy/build-release.sh [target-triple]
#   不给 triple 时按当前主机；否则用 rustc 目标（需已安装该 target + 对应链接器）。
#
# CPU 口径（用户 2026-11-26 定：目标就是正常 x86-64 与 aarch64，不针对特定芯片）：
#   - x86_64*  : -C target-cpu=x86-64-v2（SSE4.2/POPCNT 基线，2013+ 普通 x86-64 都能跑）
#   - aarch64* : -C target-cpu=generic（ARMv8-A 基线，NEON 是必选，大小核同二进制可跑）
#
# 交叉编译（x86_64 主机 → aarch64，apt multiarch 方案）：
#   sudo dpkg --add-architecture arm64 && sudo apt-get update
#   sudo apt-get install -y gcc-aarch64-linux-gnu libasound2-dev:arm64 pkg-config
#   rustup target add aarch64-unknown-linux-gnu
#   本脚本会自动设置 linker / pkg-config / 链接搜索路径（可用 AARCH64_LINKER 覆盖链接器）。
#
# 说明：target-cpu 只影响编译期代码生成（自动向量化/调度）；运行时 AVX2 路径由
# is_x86_feature_detected! 决定，无 AVX2 自动回退标量；aarch64 的 NEON 是基线指令。
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
HOST="$(rustc -vV | sed -n 's/^host: //p')"
TARGET="${1:-${HOST}}"

case "${TARGET}" in
  x86_64-*) CPU_FLAGS="-C target-cpu=x86-64-v2" ;;
  aarch64-*) CPU_FLAGS="-C target-cpu=generic" ;;
  *) echo "unsupported target: ${TARGET} (expect x86_64-* or aarch64-*)" >&2; exit 2 ;;
esac

if [[ "${TARGET}" != "${HOST}" ]]; then
  case "${TARGET}" in
    aarch64-*)
      LINKER="${AARCH64_LINKER:-aarch64-linux-gnu-gcc}"
      if ! command -v "${LINKER}" >/dev/null 2>&1; then
        echo "missing cross linker: ${LINKER} (apt install gcc-aarch64-linux-gnu)" >&2
        exit 2
      fi
      export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="${LINKER}"
      # 让 alsa-sys 的 pkg-config 走 arm64 multiarch 的 .pc 与库路径
      # arm64 头/库位置：本机多架构安装在 /usr/lib/aarch64-linux-gnu；
      # CI 用解包的交叉 sysroot（AARCH64_SYSROOT=/opt/arm64-sysroot）避开 apt 版本错位。
      if [[ -n "${AARCH64_SYSROOT:-}" ]]; then
        ARM_LIB_DIR="${AARCH64_SYSROOT}/usr/lib/aarch64-linux-gnu"
        ARM_SYSROOT="${AARCH64_SYSROOT}"
      else
        ARM_LIB_DIR="/usr/lib/aarch64-linux-gnu"
        ARM_SYSROOT="/"
      fi
      export PKG_CONFIG_ALLOW_CROSS=1
      export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-${ARM_LIB_DIR}/pkgconfig:/usr/share/pkgconfig}"
      export PKG_CONFIG_LIBDIR="${PKG_CONFIG_LIBDIR:-${ARM_LIB_DIR}/pkgconfig:/usr/share/pkgconfig}"
      export PKG_CONFIG_SYSROOT_DIR="${PKG_CONFIG_SYSROOT_DIR:-${ARM_SYSROOT}}"
      # 交叉 gcc 默认 sysroot 里没有 arm64 的 ALSA 库，显式给链接器搜索路径
      CPU_FLAGS="${CPU_FLAGS} -C link-arg=-L${ARM_LIB_DIR}"
      ;;
    *)
      echo "no cross recipe for ${TARGET} (only aarch64 from x86_64 is supported)" >&2
      exit 2
      ;;
  esac
fi

# 构建期版本注入（daemon/build.rs → /healthz 的 version；CI tag 构建会预先导出）：
# 本地默认取根 Cargo.toml 的 workspace version 与 git HEAD，保证 /healthz 报 release 版本而不是 crate 版本。
if [[ -z "${KHAEN_BUILD_VERSION:-}" ]]; then
  KHAEN_BUILD_VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d'\"' -f2)"
  export KHAEN_BUILD_VERSION
fi
if [[ -z "${KHAEN_BUILD_COMMIT:-}" ]]; then
  KHAEN_BUILD_COMMIT="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
  export KHAEN_BUILD_COMMIT
fi
echo "==> build version ${KHAEN_BUILD_VERSION} commit ${KHAEN_BUILD_COMMIT:0:12}"

echo "==> build ${TARGET} on ${HOST} with RUSTFLAGS='${CPU_FLAGS}'"
export RUSTFLAGS="${CPU_FLAGS}${RUSTFLAGS:+ ${RUSTFLAGS}}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}"

cargo build --release --locked \
  --target "${TARGET}" \
  -p khaen-daemon -p khaen-rt-host -p khaen-cal-tools -p khaen-calibration -p khaen-analyze

echo "==> built: ${CARGO_TARGET_DIR}/${TARGET}/release"
