#!/usr/bin/env bash
# build-all.sh — 顺序构建全部音频栈产物，并生成 release/MANIFEST.sha256
#
# 用法：
#   ./build-all.sh [--method native|cross] [--kernel-dir DIR] [--mock-aes67] [--skip-aes67] [--check]
#
# 产物目录：
#   release/bin/        camilladsp inferno2pipe statime aes67-daemon linuxptp/{ptp4l,phc2sys,pmc}
#   release/lib/        libasound_module_pcm_inferno*.so
#   release/modules/    MergingRavennaALSA.ko
#   release/debs/       fetch-debs.sh 抓的离线 .deb（install-into-image.sh 用）
#   release/src/        源码/官方资产缓存（可复用/离线构建）
#   release/build-info.txt / MANIFEST.sha256
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/build-common.sh"

METHOD="native"
KERNEL_DIR="/lib/modules/$(uname -r)/build"
MOCK_AES67=0
SKIP_AES67=0
CHECK_ONLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --method) METHOD="$2"; shift 2 ;;
    --kernel-dir) KERNEL_DIR="$2"; shift 2 ;;
    --mock-aes67) MOCK_AES67=1; shift ;;
    --skip-aes67) SKIP_AES67=1; shift ;;
    --check) CHECK_ONLY=1; shift ;;
    -h|--help) sed -n "2,13p" "$0"; exit 0 ;;
    *) die "unknown option: $1" 2 ;;
  esac
done

verify_all() {
  local fail=0
  local f
  for f in camilladsp inferno2pipe statime aes67-daemon; do
    if [[ -x "$BIN_DIR/$f" ]]; then log "OK  bin/$f"; else warn "MISSING bin/$f"; fail=1; fi
  done
  if compgen -G "$LIB_DIR/libasound_module_pcm_inferno*.so" >/dev/null; then log "OK  lib/alsa plugin"; else warn "MISSING lib/alsa plugin"; fail=1; fi
  if [[ -f "$MOD_DIR/MergingRavennaALSA.ko" ]]; then log "OK  modules/MergingRavennaALSA.ko"; else warn "MISSING modules/MergingRavennaALSA.ko"; fail=1; fi
  for b in ptp4l phc2sys pmc; do
    if [[ -x "$BIN_DIR/linuxptp/$b" ]]; then log "OK  bin/linuxptp/$b"; else warn "MISSING bin/linuxptp/$b"; fail=1; fi
  done
  if [[ -d "$REL_DIR/debs" ]] && compgen -G "$REL_DIR/debs/*.deb" >/dev/null; then
    log "OK  debs: $(ls -1 "$REL_DIR/debs"/*.deb | wc -l) 个（install-into-image.sh 用）"
  else
    warn "release/debs 为空：先跑 ./fetch-debs.sh（镜像离线安装需要）"
  fi
  return "$fail"
}

write_manifest() {
  local out="$REL_DIR/MANIFEST.sha256"
  : > "$out"
  ( cd "$REL_DIR" && find bin lib modules -maxdepth 2 -type f ! -name "*.sha256" -print0 2>/dev/null \
      | sort -z | xargs -0 -r sha256sum ) >> "$out" || true
  if [[ -d "$REL_DIR/debs" ]]; then
    ( cd "$REL_DIR" && find debs -maxdepth 1 -name "*.deb" -print0 2>/dev/null | sort -z | xargs -0 -r sha256sum ) >> "$out" || true
  fi
  log "MANIFEST: $out ($(wc -l < "$out") 行)"
}

if [[ "$CHECK_ONLY" == 1 ]]; then
  verify_all || die "产物不完整（见上）" 4
  write_manifest
  exit 0
fi

log "== 1/4 linuxptp（默认 bookworm .deb）"
"$PROJ_ROOT/build-linuxptp.sh"
log "== 2/4 CamillaDSP"
"$PROJ_ROOT/build-camilladsp.sh" --method "$METHOD"
log "== 3/4 Inferno + Statime"
"$PROJ_ROOT/build-inferno.sh" --method "$METHOD"
if [[ "$SKIP_AES67" == 1 ]]; then
  warn "== 4/4 跳过 aes67-daemon（--skip-aes67）"
else
  log "== 4/4 aes67-daemon + RAVENNA LKM"
  if [[ "$MOCK_AES67" == 1 ]]; then
    "$PROJ_ROOT/build-aes67-daemon.sh" --mock
  else
    "$PROJ_ROOT/build-aes67-daemon.sh" --kernel-dir "$KERNEL_DIR"
  fi
fi

verify_all || warn "部分产物缺失（见上）；镜像集成前必须补齐"
write_manifest
record "build-all: done method=$METHOD ai=$(uname -m) kernel=$(uname -r)"
log "下一步（在 Debian rootfs 上）：sudo ./install-into-image.sh --rootfs <dir> --overlay ./overlay --release ./release --debs ./release/debs --kernel <kernel build dir>"
