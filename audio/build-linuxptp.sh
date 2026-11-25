#!/usr/bin/env bash
# build-linuxptp.sh — linuxptp（ptp4l/phc2sys/pmc，aarch64）
#
# 默认方式 debian-package：用 Debian 12 bookworm 3.1.1-4 的 .deb（由 fetch-debs.sh 抓），
#   解包到 release/bin/linuxptp/ 并校验；镜像里由 install-into-image.sh 安装 .deb。
# 可选 --source：从上游源码编译指定 tag（默认 v3.1.1，与 Debian 版本一致；也可 v4.2 但需重测配置兼容）。
#
# 用法：./build-linuxptp.sh [--mode debian-package|source] [--tag v3.1.1] [--debs release/debs]
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/build-common.sh"

MODE="debian-package"
TAG="v3.1.1"
DEBS_DIR="$REL_DIR/debs"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --debs) DEBS_DIR="$2"; shift 2 ;;
    -h|--help) sed -n "2,9p" "$0"; exit 0 ;;
    *) die "unknown option: $1" 2 ;;
  esac
done

record "linuxptp: mode=$MODE tag=$TAG"
OUTD="$BIN_DIR/linuxptp"
mkdir -p "$OUTD"

if [[ "$MODE" == "debian-package" ]]; then
  DEB="$(ls -1 "$DEBS_DIR"/linuxptp_*.deb 2>/dev/null | head -1 || true)"
  [[ -n "$DEB" ]] || die "找不到 linuxptp .deb；先运行 ./fetch-debs.sh（或 --mode source）" 3
  log "using deb: $DEB"
  dpkg-deb -c "$DEB" | awk '{print $NF}' | grep -E '/(ptp4l|phc2sys|pmc)$' || die ".deb 内缺少 ptp4l/phc2sys/pmc: $DEB" 3
  TMPD="$(mktemp -d)"; trap "rm -rf "$TMPD"" EXIT
  dpkg-deb -x "$DEB" "$TMPD"
  for b in ptp4l phc2sys pmc; do
    install_artifact "$TMPD/usr/sbin/$b" "$OUTD/$b" 755
  done
  record "linuxptp: from deb $(basename "$DEB") sha256=$(sha_of "$DEB")"
else
  need_cmd git; need_cmd make; need_cmd gcc
  SRCD="$SRC_DIR/linuxptp-$TAG"
  if [[ ! -d "$SRCD/.git" ]]; then
    git clone --depth 1 --branch "$TAG" https://git.code.sf.net/p/linuxptp/code "$SRCD" \
      || git clone --depth 1 --branch "$TAG" https://github.com/richardcochran/linuxptp.git "$SRCD"
  fi
  ( cd "$SRCD" && make -j"$(nproc)" )
  for b in ptp4l phc2sys pmc; do
    install_artifact "$SRCD/$b" "$OUTD/$b" 755
  done
  record "linuxptp: built from $TAG at $SRCD"
fi

check_elf_aarch64 "$OUTD/ptp4l"
check_elf_aarch64 "$OUTD/phc2sys"
print_summary
