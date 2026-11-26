#!/usr/bin/env bash
# khaen 单点闭环复测判定包装（设备侧）
#
# 状态：NOT_RUN —— 从未在真实设备上执行过；语法已用 bash -n 检查，底层命令已在离线测试里验证。
# 本脚本只做参数检查并调用 khaen-closed-loop-run；不写 CueMix、不改 ALSA 音量、不重启服务。
#
# 用法：
#   deploy/closed-loop-verdict.sh --plan plan.bin --baseline baseline.wav \
#       --applied applied.wav --out /var/lib/khaen/closed-loop/<run> \
#       [--point P01] [--lane 0] [--centers 25,31.5,40,...] [--window 4096] \
#       [--tolerance 2.0] [--min-bands 3]
#
# 环境变量：
#   KHAEN_CLOSED_LOOP_RUN  可执行文件路径（默认从 PATH 找 khaen-closed-loop-run）
#
# 退出码（与 khaen-closed-loop-run 一致）：
#   0 PASS / 3 FAIL / 4 INCOMPLETE / 2 输入或方案错误 / 1 写输出失败
# 输出目录必须为空或不存在；产物 predicted.json / observed.json / report.json / manifest.json。

set -euo pipefail

BIN="${KHAEN_CLOSED_LOOP_RUN:-khaen-closed-loop-run}"
PLAN=""
BASELINE=""
APPLIED=""
OUT=""
POINT="P01"
LANE="0"
CENTERS=""
WINDOW=""
TOLERANCE="2.0"
MIN_BANDS="3"

usage() {
  echo "usage: closed-loop-verdict.sh --plan FILE --baseline WAV --applied WAV --out DIR" >&2
  echo "       [--point P01] [--lane 0] [--centers a,b,c] [--window 4096]" >&2
  echo "       [--tolerance 2.0] [--min-bands 3]" >&2
}

fail() {
  echo "closed-loop-verdict: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan)
      [ "$#" -ge 2 ] || fail "--plan needs a value"
      PLAN="$2"
      shift 2
      ;;
    --baseline)
      [ "$#" -ge 2 ] || fail "--baseline needs a value"
      BASELINE="$2"
      shift 2
      ;;
    --applied)
      [ "$#" -ge 2 ] || fail "--applied needs a value"
      APPLIED="$2"
      shift 2
      ;;
    --out)
      [ "$#" -ge 2 ] || fail "--out needs a value"
      OUT="$2"
      shift 2
      ;;
    --point)
      [ "$#" -ge 2 ] || fail "--point needs a value"
      POINT="$2"
      shift 2
      ;;
    --lane)
      [ "$#" -ge 2 ] || fail "--lane needs a value"
      LANE="$2"
      shift 2
      ;;
    --centers)
      [ "$#" -ge 2 ] || fail "--centers needs a value"
      CENTERS="$2"
      shift 2
      ;;
    --window)
      [ "$#" -ge 2 ] || fail "--window needs a value"
      WINDOW="$2"
      shift 2
      ;;
    --tolerance)
      [ "$#" -ge 2 ] || fail "--tolerance needs a value"
      TOLERANCE="$2"
      shift 2
      ;;
    --min-bands)
      [ "$#" -ge 2 ] || fail "--min-bands needs a value"
      MIN_BANDS="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

if [ -z "$PLAN" ] || [ -z "$BASELINE" ] || [ -z "$APPLIED" ] || [ -z "$OUT" ]; then
  usage
  fail "--plan / --baseline / --applied / --out are required"
fi

if ! command -v "$BIN" >/dev/null 2>&1; then
  fail "khaen-closed-loop-run not found: set KHAEN_CLOSED_LOOP_RUN or add it to PATH"
fi

if [ ! -f "$PLAN" ]; then fail "plan file not found: $PLAN"; fi
if [ ! -f "$BASELINE" ]; then fail "baseline wav not found: $BASELINE"; fi
if [ ! -f "$APPLIED" ]; then fail "applied wav not found: $APPLIED"; fi
if [ -e "$OUT" ] && [ -n "$(find "$OUT" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  fail "output directory is not empty: $OUT"
fi

ARGS=(--plan "$PLAN" --baseline "$BASELINE" --applied "$APPLIED" --out "$OUT")
ARGS+=(--point "$POINT" --lane "$LANE" --tolerance "$TOLERANCE" --min-bands "$MIN_BANDS")
if [ -n "$CENTERS" ]; then ARGS+=(--centers "$CENTERS"); fi
if [ -n "$WINDOW" ]; then ARGS+=(--window "$WINDOW"); fi

set +e
"$BIN" "${ARGS[@]}"
CODE=$?
set -e

case "$CODE" in
  0) echo "closed-loop-verdict: PASS（四份产物在 $OUT）" ;;
  3) echo "closed-loop-verdict: FAIL —— 预测与实测超容差，报告在 $OUT/report.json" >&2 ;;
  4) echo "closed-loop-verdict: INCOMPLETE —— 可比较带不足，报告在 $OUT/report.json" >&2 ;;
  2) echo "closed-loop-verdict: 输入或方案错误（见上方稳定错误码）" >&2 ;;
  1) echo "closed-loop-verdict: 写输出失败" >&2 ;;
  *) echo "closed-loop-verdict: unexpected exit code $CODE" >&2 ;;
esac

exit "$CODE"
