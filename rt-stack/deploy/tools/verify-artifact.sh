#!/usr/bin/env bash
# T149/T71 可部署产物校验（ELF aarch64 / NEEDED / GLIBC 版本 / sha256 / size / qemu selftest）
#
# 用法（Linux 构建机或板端执行）：
#   ./verify-artifact.sh <binary> [--sysroot DIR] [--max-glibc 2.36] [--require-libasound 1]
#                        [--qemu PATH] [--selftest] [--json OUT]
# 说明：
#   * 本机（Windows 开发机）用 deploy/lib/check_elf.py 做 ELF 头检查；本脚本用于 Linux 构建机做完整 readelf/glibc 校验。
#   * 板端 Debian 12 (bookworm) glibc=2.36；产物 GLIBC 符号版本不得高于 2.36，否则拒绝部署。
#   * NEEDED 必须含 libasound.so.2（rt-host 真实 ALSA 后端）与 libc.so.6；解释器必须是 /lib/ld-linux-aarch64.so.1。
# 退出码：0 PASS；2 用法/文件缺失；3 FAIL（架构/依赖/glibc 不满足）。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN=""; SYSROOT=""; MAX_GLIBC="2.36"; REQ_ALSA=1; QEMU=""; DO_SELFTEST=0; JSON_OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --sysroot) SYSROOT="$2"; shift 2 ;;
    --max-glibc) MAX_GLIBC="$2"; shift 2 ;;
    --require-libasound) REQ_ALSA="$2"; shift 2 ;;
    --qemu) QEMU="$2"; shift 2 ;;
    --selftest) DO_SELFTEST=1; shift ;;
    --json) JSON_OUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) BIN="$1"; shift ;;
  esac
done
[ -n "$BIN" ] || { echo 'usage: verify-artifact.sh <binary> [opts]' >&2; exit 2; }
[ -f "$BIN" ] || { echo "missing binary: $BIN" >&2; exit 2; }
FAILS=0; WARNS=0
say() { printf '[verify] %s\n' "$*" >&2; }
fail() { FAILS=$((FAILS+1)); say "FAIL: $*"; }
warn() { WARNS=$((WARNS+1)); say "WARN: $*"; }
pass() { say "PASS: $*"; }
READELF="$(command -v aarch64-linux-gnu-readelf || command -v readelf || true)"
[ -n "$READELF" ] || { echo 'missing readelf/aarch64-linux-gnu-readelf' >&2; exit 3; }
SHA="$(sha256sum "$BIN" | awk '{print $1}')"
SIZE="$(stat -c %s "$BIN")"
say "binary=$BIN size=$SIZE sha256=$SHA readelf=$READELF"
# ---- file/ELF 头 ----
if command -v file >/dev/null 2>&1; then
  FT="$(file -b "$BIN")"; say "file: $FT"
  case "$FT" in *ELF*64-bit*ARM*aarch64*) pass "file 显示 ELF64 aarch64" ;; *) fail "file 不是 ELF64 aarch64: $FT" ;; esac
else
  warn "file 命令缺失（跳过 file 文本检查）"
fi
CLASS="$($READELF -h "$BIN" 2>/dev/null | awk -F: '/Class:/{gsub(/^ +/,"",$2);print $2}')"
MACHINE="$($READELF -h "$BIN" 2>/dev/null | awk -F: '/Machine:/{gsub(/^ +/,"",$2);print $2}')"
ETYPE="$($READELF -h "$BIN" 2>/dev/null | awk -F: '/Type:/{gsub(/^ +/,"",$2);print $2}')"
EDATA="$($READELF -h "$BIN" 2>/dev/null | awk -F: '/Data:/{gsub(/^ +/,"",$2);print $2}')"
say "elf: class=$CLASS machine=$MACHINE type=$ETYPE data=$EDATA"
[ "$CLASS" = "ELF64" ] && pass "ELF64" || fail "Class=$CLASS（要求 ELF64）"
case "$MACHINE" in *AArch64*|*aarch64*) pass "machine=AArch64" ;; *) fail "Machine=$MACHINE（要求 AArch64）" ;; esac
case "$ETYPE" in *DYN*|*EXEC*) pass "Type=$ETYPE" ;; *) fail "Type=$ETYPE（要求 DYN/EXEC）" ;; esac
case "$EDATA" in *little*|*2*) pass "little-endian" ;; *) warn "Data=$EDATA（预期小端）" ;; esac
# ---- 解释器 ----
INTERP="$($READELF -l "$BIN" 2>/dev/null | grep -oE '/lib/ld-linux-aarch64\.so\.1|/lib64/ld-linux-aarch64\.so\.1' | head -n 1)"
[ -n "$INTERP" ] && pass "interpreter=$INTERP" || fail "缺动态解释器 /lib/ld-linux-aarch64.so.1（静态/错架构？）"
# ---- NEEDED ----
NEEDED="$($READELF -d "$BIN" 2>/dev/null | awk -F'[][]' '/NEEDED/{print $2}')"
say "NEEDED: $(printf '%s ' $NEEDED)"
if printf '%s' "$NEEDED" | grep -qx 'libc.so.6'; then pass "NEEDED libc.so.6"; else fail "NEEDED 缺 libc.so.6"; fi
if [ "$REQ_ALSA" = "1" ]; then
  if printf '%s' "$NEEDED" | grep -qx 'libasound.so.2'; then pass "NEEDED libasound.so.2（真实 ALSA 后端）"; else fail "NEEDED 缺 libasound.so.2（real-alsa 未链接）"; fi
fi
if printf '%s' "$NEEDED" | grep -q 'libasound_module_pcm_inferno'; then warn "NEEDED 含 inferno 插件（板端仅 AoIP 模式需要；确认加载路径）"; fi
printf '%s' "$NEEDED" | grep -q 'libstdc++\|libssl\|libsqlite' && warn "NEEDED 含 C++/SSL/sqlite（确认板端有对应库）" || true
# ---- GLIBC 版本 ----
GLIBC_MAX="$($READELF --version-info "$BIN" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' | sort -u -V | tail -n 1 | sed 's/GLIBC_//')"
[ -n "$GLIBC_MAX" ] || GLIBC_MAX="$($READELF --version-info "$BIN" 2>/dev/null | grep -oE 'GLIBC_[0-9.]+' | sort -u -V | tail -n 1 | sed 's/GLIBC_//')"
say "GLIBC max=$GLIBC_MAX（板端 bookworm=2.36）"
if [ -n "$GLIBC_MAX" ]; then
  NEWEST="$(printf '%s\n%s\n' "$MAX_GLIBC" "$GLIBC_MAX" | sort -V | tail -n 1)"
  if [ "$NEWEST" = "$MAX_GLIBC" ]; then pass "GLIBC $GLIBC_MAX <= 板端 $MAX_GLIBC"; else fail "GLIBC $GLIBC_MAX > 板端 $MAX_GLIBC"; fi
else warn "未解析到 GLIBC 符号版本（静态或异常）"; fi
ALSA_VER="$($READELF --version-info "$BIN" 2>/dev/null | grep -oE 'ALSA_[0-9.]+' | sort -u -V | tail -n 1)"
[ -n "$ALSA_VER" ] && say "ALSA 符号版本 max=$ALSA_VER（板端 libasound 1.2.8）" || true
# ---- sysroot 兼容（可选） ----
if [ -n "$SYSROOT" ]; then
  for lib in libc.so.6 libasound.so.2; do
    if [ -e "$SYSROOT/lib/aarch64-linux-gnu/$lib" ] || [ -e "$SYSROOT/usr/lib/aarch64-linux-gnu/$lib" ]; then pass "sysroot 含 $lib"; else fail "sysroot 缺 $lib"; fi
  done
fi
# ---- qemu selftest（可选） ----
SELFTEST_JSON='"not_run"'
if [ "$DO_SELFTEST" = "1" ]; then
  if [ -z "$QEMU" ] || [ ! -x "$QEMU" ]; then warn "--selftest 但 qemu 不可用"; SELFTEST_JSON='"qemu_missing"'
  elif [ -z "$SYSROOT" ]; then warn "--selftest 需要 --sysroot"; SELFTEST_JSON='"sysroot_missing"'
  else
    if "$QEMU" -L "$SYSROOT" "$BIN" selftest > /tmp/khaen-artifact-selftest.json 2>/tmp/khaen-artifact-selftest.err; then
      pass "qemu selftest PASS"; SELFTEST_JSON="$(cat /tmp/khaen-artifact-selftest.json)"
    else
      fail "qemu selftest FAIL"; SELFTEST_JSON='"fail"'
    fi
  fi
fi
JSON="{\"schema\":\"khaen-rt-stack/artifact-check/v1\",\"binary\":\"$(basename "$BIN")\",\"size\":$SIZE,\"sha256\":\"$SHA\",\"elf_class\":\"$CLASS\",\"machine\":\"$MACHINE\",\"type\":\"$ETYPE\",\"interp\":\"$INTERP\",\"needed\":[$(printf '%s' "$NEEDED" | sed 's/.*/"&"/' | paste -sd, -)],\"glibc_max\":\"$GLIBC_MAX\",\"alsa_version_max\":\"$ALSA_VER\",\"selftest\":$SELFTEST_JSON,\"fails\":$FAILS,\"warns\":$WARNS}"
if [ -n "$JSON_OUT" ]; then printf '%s\n' "$JSON" > "$JSON_OUT"; fi
say "json: $JSON"
say "RESULT: $( [ "$FAILS" -eq 0 ] && echo PASS || echo FAIL ) fails=$FAILS warns=$WARNS"
[ "$FAILS" -eq 0 ] && exit 0 || exit 3
