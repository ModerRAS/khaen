#!/usr/bin/env bash
# build-camilladsp.sh — CamillaDSP v4.1.3（aarch64）构建 / 官方资产获取
#
# 产物：release/bin/camilladsp
# 方式：
#   --method official  官方 release 资产 camilladsp-linux-aarch64.tar.gz（ALSA-only，推荐兜底）
#   --method native    本机（aarch64）cargo build --release --locked
#   --method cross     cross build --target aarch64-unknown-linux-gnu
#   --method auto      有 cargo 且本机 aarch64 → native；否则 official
#
# 用法：./build-camilladsp.sh [--version v4.1.3] [--method auto|official|native|cross] [--offline]
# 依赖：curl（official 下载）；cargo/rustup（native；Rust >= 1.90）；cross（cross）
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/build-common.sh"

VERSION="v4.1.3"
METHOD="auto"
OFFLINE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --method) METHOD="$2"; shift 2 ;;
    --offline) OFFLINE=1; shift ;;
    -h|--help) sed -n "2,12p" "$0"; exit 0 ;;
    *) die "unknown option: $1" 2 ;;
  esac
done

if [[ "$METHOD" == "auto" ]]; then
  if command -v cargo >/dev/null 2>&1 && [[ "$(uname -m)" == "aarch64" ]] && ! command -v cross >/dev/null 2>&1; then
    METHOD="native"
  elif [[ "$(uname -m)" == "aarch64" ]] && command -v cargo >/dev/null 2>&1; then
    METHOD="native"
  else
    METHOD="official"
  fi
fi
log "CamillaDSP $VERSION method=$METHOD"
record "camilladsp: version=$VERSION method=$METHOD"

case "$METHOD" in
  official)
    ART="$SRC_DIR/camilladsp-$VERSION-linux-aarch64.tar.gz"
    [[ "$OFFLINE" == 1 && ! -f "$ART" ]] && die "offline 模式需要预置 $ART" 3
    if [[ ! -f "$ART" ]]; then
      download "https://github.com/HEnquist/camilladsp/releases/download/$VERSION/camilladsp-linux-aarch64.tar.gz" "$ART"
    fi
    log "asset sha256: $(sha_of "$ART")  size: $(stat -c %s "$ART")"
    TMPD="$(mktemp -d)"; trap "rm -rf "$TMPD"" EXIT
    tar -xzf "$ART" -C "$TMPD"
    install_artifact "$TMPD/camilladsp" "$BIN_DIR/camilladsp" 755
    ;;
  native)
    need_cmd cargo
    RUSTC_V="$(rustc -V | awk '{print $2}')"
    log "rustc: $RUSTC_V"
    SRCD="$SRC_DIR/camilladsp-$VERSION"
    if [[ ! -d "$SRCD/.git" ]]; then
      [[ "$OFFLINE" == 1 ]] && die "offline 模式需要预置源码 $SRCD" 3
      need_cmd git
      git clone --depth 1 --branch "$VERSION" https://github.com/HEnquist/camilladsp.git "$SRCD"
    fi
    ( cd "$SRCD" && cargo build --release --locked )
    install_artifact "$SRCD/target/release/camilladsp" "$BIN_DIR/camilladsp" 755
    record "camilladsp: rustc=$RUSTC_V source=$SRCD"
    ;;
  cross)
    need_cmd cross
    SRCD="$SRC_DIR/camilladsp-$VERSION"
    [[ -d "$SRCD/.git" ]] || die "cross 模式需要源码，请先以 native 模式准备（或手动 clone 到 $SRCD）" 3
    ( cd "$SRCD" && cross build --release --locked --target aarch64-unknown-linux-gnu )
    install_artifact "$SRCD/target/aarch64-unknown-linux-gnu/release/camilladsp" "$BIN_DIR/camilladsp" 755
    ;;
  *) die "unknown method: $METHOD" 2 ;;
esac

check_elf_aarch64 "$BIN_DIR/camilladsp"
if [[ "$(uname -m)" == "aarch64" ]]; then
  "$BIN_DIR/camilladsp" --version || die "camilladsp --version 失败"
  record "camilladsp: self-test --version OK"
fi
print_summary
