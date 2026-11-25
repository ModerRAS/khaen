#!/usr/bin/env bash
# 交叉编译 khaen-audio-web 到 aarch64（Debian 12 / RK3576）。
#
# 在 Linux 构建机（x86_64）上执行：
#   sudo apt install -y gcc-aarch64-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
#   ./scripts/cross-build.sh
#
# 也可以直接在板端/同架构容器里原生构建：
#   cargo build --locked --release
#
# 环境变量：
#   TARGET   目标 triple（默认 aarch64-unknown-linux-gnu）
#   PROFILE  release / debug（默认 release）
#   CARGO_TARGET_DIR  自定义构建目录（默认 target/，不要指向易失内存盘）
set -euo pipefail

TARGET="${TARGET:-aarch64-unknown-linux-gnu}"
PROFILE="${PROFILE:-release}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: 未找到 cargo，请先安装 Rust（https://rustup.rs）" >&2
  exit 1
fi

if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  echo "==> 安装 rust target: $TARGET"
  rustup target add "$TARGET"
fi

# 自动探测交叉链接器（CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER 可显式指定）
LINKER_VAR="CARGO_TARGET_$(echo "$TARGET" | tr '[:lower:]-' '[:upper:]_')_LINKER"
if [ -z "${!LINKER_VAR:-}" ]; then
  for candidate in aarch64-linux-gnu-gcc aarch64-none-linux-gnu-gcc; do
    if command -v "$candidate" >/dev/null 2>&1; then
      export "$LINKER_VAR=$candidate"
      echo "==> 使用链接器: $candidate"
      break
    fi
  done
fi

if [ "$TARGET" = "aarch64-unknown-linux-gnu" ] && [ -z "${!LINKER_VAR:-}" ]; then
  echo "warn: 未找到 aarch64-linux-gnu-gcc，请安装 gcc-aarch64-linux-gnu 或设置 $LINKER_VAR" >&2
fi

echo "==> cargo build --locked --target $TARGET --profile $PROFILE"
cargo build --locked --target "$TARGET" --profile "$PROFILE"

BIN="$ROOT/target/$TARGET/$PROFILE/khaen-audio-web"
if [ ! -f "$BIN" ]; then
  echo "error: 构建产物不存在: $BIN" >&2
  exit 1
fi

echo
echo "==> 产物"
ls -lh "$BIN"
file "$BIN" 2>/dev/null || true
sha256sum "$BIN"
echo
echo "安装到设备（或镜像 rootfs）："
echo "  sudo ./scripts/install-device.sh --binary $BIN"
echo "  # OEM 镜像预装："
echo "  sudo ./scripts/install-device.sh --binary $BIN --rootfs /path/to/rootfs"
