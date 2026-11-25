#!/usr/bin/env bash
# T28 02-check-device-exclusive.sh —— 音频设备独占检查（**只读**；不打开 PCM）
#
# 判据：
#   1 旧音频 unit（retire+replace+retain）全部 inactive（切换窗口 audio-off）
#   2 旧 C 进程不存在（camilladsp/tspi-upmix）
#   3 /dev/snd 无其它进程持有（fuser/lsof/proc fd 三选一有输出即 FAIL）
#   4 /proc/asound/card*/pcm*/sub*/status 全部 closed（有 RUNNING/OPEN 即 FAIL）
#   5 /run/tspi/mode 非 dante/aes67
#   6 旧控制面端口（1234 Camilla WS / 80 旧 Web）无监听
#   7 MOTU card 已枚举（--no-require-motu 可关；仅用于“先证明无 owner，再查设备”的场景）
#
# 用法: sudo ./02-check-device-exclusive.sh --live [--root DIR] [--no-require-motu]
# 退出码: 0 独占可用；3 被占用/不满足；7 环境不支持。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

REQUIRE_MOTU=1
usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --no-require-motu) REQUIRE_MOTU=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 将检查：旧 units inactive / 无 C 进程 / /dev/snd 无持有 / PCM status=closed / mode 非 dante|aes67 / 1234:80 无监听 / MOTU 已枚举"
  exit 0
fi

FAILS=0
pass() { log "[PASS] $*"; }
fail() { FAILS=$((FAILS+1)); warn "[FAIL] $*"; }

# 1) units
BUSY=()
for u in "${ALL_OLD_AUDIO_UNITS[@]}"; do sunit_active "$u" && BUSY+=("$u"); done
[ "${#BUSY[@]}" -eq 0 ] && pass "旧音频 unit 全部 inactive" || fail "仍 active: ${BUSY[*]}"

# 2) C 进程
PROCS="$(pgrep -a -f 'camilladsp|tspi-upmix' 2>/dev/null || true)"
[ -z "$PROCS" ] && pass "无 camilladsp/tspi-upmix 进程" || fail "旧 C 进程仍在: $(printf '%s' "$PROCS" | head -n 3 | tr '\n' ' ')"

# 3) /dev/snd holders
SND_DIR="$(p /dev/snd)"
HOLDERS=""
if command -v fuser >/dev/null 2>&1 && [ -e "$SND_DIR" ]; then
  HOLDERS="$(fuser -v "$SND_DIR"/* 2>&1 | grep -v '^$' || true)"
fi
if [ -z "$HOLDERS" ] && command -v lsof >/dev/null 2>&1; then
  HOLDERS="$(lsof "$SND_DIR"/* 2>/dev/null | awk 'NR>1{print $1,$2}' || true)"
fi
if [ -z "$HOLDERS" ]; then
  # 不依赖外部命令：扫 /proc/<pid>/fd 链接（root/板端可行；夹具下为空）
  PFX="$(p /proc)"
  HOLDERS="$(for fd in "$PFX"/[0-9]*/fd/*; do [ -L "$fd" ] || continue; tgt="$(readlink "$fd" 2>/dev/null || true)"; case "$tgt" in /dev/snd/*) printf '%s -> %s\n' "$fd" "$tgt";; esac; done 2>/dev/null | head -n 20 || true)"
fi
[ -z "$HOLDERS" ] && pass "/dev/snd 无进程持有（fuser/lsof/proc 扫描）" || fail "/dev/snd 被占用: $(printf '%s' "$HOLDERS" | head -n 5 | tr '\n' ' ')"

# 4) PCM status
OPEN=""
for f in "$(p /proc)"/asound/card*/pcm*/sub*/status; do
  [ -r "$f" ] || continue
  st="$(tr -d '[:space:]' < "$f" 2>/dev/null || true)"
  [ "$st" = "closed" ] || OPEN="$OPEN $f=$st"
done
[ -z "$OPEN" ] && pass "所有 PCM substream status=closed" || fail "PCM 非 closed:$OPEN"

# 5) mode
MODE_FILE="$(p /run/tspi/mode)"
if [ -f "$MODE_FILE" ]; then
  MODE="$(tr -d '[:space:]' < "$MODE_FILE" 2>/dev/null || true)"
  case "$MODE" in dante|aes67) fail "/run/tspi/mode=$MODE";; *) pass "/run/tspi/mode=${MODE:-unknown}";; esac
else
  pass "/run/tspi/mode 不存在（audio-off）"
fi

# 6) old control ports
LISTEN="$(ss -ltnp 2>/dev/null | grep -E ':(1234|80)\\b' || true)"
if [ -n "$LISTEN" ]; then
  # 80 可能被保留 Web/系统服务占用；列出供人工裁决，但按设备独占门 FAIL（旧控制面必须先停）
  fail "旧控制面端口仍监听（1234/80）: $(printf '%s' "$LISTEN" | head -n 3 | tr '\n' ' ')"
else
  pass "1234/80 无监听（旧 Camilla WS/旧 Web 已停）"
fi

# 7) MOTU
if [ "$REQUIRE_MOTU" = "1" ]; then
  if grep -qi 'UltraLite' "$(p /proc/asound/cards)" 2>/dev/null; then pass "MOTU UltraLite 已枚举"; else fail "MOTU UltraLite 未枚举"; fi
fi

if [ "$FAILS" -gt 0 ]; then die "设备独占门失败 $FAILS 项：不得启动新 host（旧 C 保持原状态/已 masked 状态不变）" 3; fi
exit 0
