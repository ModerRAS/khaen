#!/usr/bin/env bash
# T72 板端只读检查：无 CamillaDSP / 旧 DSP 运行依赖（G-no-C 运行时证明）
#
# 只读：不 stop/start/enable/disable、不写板端、不打开 PCM、不播放、不改配置。
# 输出：JSON（机器可读、可作前后快照）+ 人类可读摘要；供 T72 验收与 T71 窗口前后对比。
#
# 检查项（硬门 FAIL -> exit 5）：
#   N1 进程      ps 等价（/proc 扫描）无 camilladsp / tspi-upmix / khaen-audio-web / tspi-mode / sox DSP 进程
#   N2 旧 unit   旧音频 unit 不存在，或 disabled/masked 且 inactive（active/enabled 即 FAIL）
#   N3 cgroup    任何进程 cgroup / system.slice 无旧 unit（camilladsp/tspi-upmix/khaen-audio-web）
#   N4 旧端口    1234（CamillaDSP WS）与 18080 无 LISTEN，也无连接
#   N5 旧入口    /run/tspi/camilladsp.{yml,env}、/etc/tspi/last-mode、/run/tspi/mode 均不存在
#   N6 socket/pipe  无 camilladsp 命名 socket/FIFO；无进程 fd 指向 camilladsp/tspi-upmix 或旧 /run/tspi 路径
#   N7 PCM owner 新 host 是 MOTU PCM 唯一 owner（/dev/snd 持有者仅 host；/proc/asound owner_pid 仅 host）
#   N8 新栈      khaen-rt-host 单实例 active，exe 位于 /opt/tspi-rt-stack 且记录 sha256（--require-daemon 时 daemon 亦 active）
#   N9 依赖入口  无 *.wants/*.requires 指向旧 unit；新栈 unit 无正向旧 C 依赖（Before= 等 ordering-only 也 FAIL，按 G-no-C）
#   N10 历史文件 旧 C 二进制/配置存在仅 INFO（历史文件保留 != 旧服务运行；不 FAIL）
#
# 用法:
#   sudo ./board_check_no_c.sh --live --phase post-switch --require-daemon \
#        --json-out /var/lib/tspi-rt-stack/evidence/no-c-proof/board-post-switch.json \
#        --human-out /var/lib/tspi-rt-stack/evidence/no-c-proof/board-post-switch.txt
#   ./board_check_no_c.sh --live --root /tmp/fixture/board ...   # 本地夹具干跑（不连板）
#   ./board_check_no_c.sh --dry-run                               # 只打印计划
#
# 退出码：0 硬门全 PASS；2 用法；5 硬门 FAIL；7 环境不支持（缺命令/非 Linux）。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="/"
LIVE=0
PHASE="post-switch"
REQUIRE_DAEMON=0
ALLOW_HOST_INACTIVE=0
ALLOW_NO_MOTU=0
REQUIRE_AUDIO_OFF=0
JSON_OUT=""
HUMAN_OUT=""
EVIDENCE_DIR=""
VERSION="t72-no-c-board-check/v1"

usage() { sed -n '2,31p' "$0" | sed 's/^# *//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --live) LIVE=1; shift ;;
    --dry-run) LIVE=0; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --phase) PHASE="$2"; shift 2 ;;
    --require-daemon) REQUIRE_DAEMON=1; shift ;;
    --allow-host-inactive) ALLOW_HOST_INACTIVE=1; shift ;;
    --allow-no-motu) ALLOW_NO_MOTU=1; shift ;;
    --require-audio-off) REQUIRE_AUDIO_OFF=1; shift ;;
    --json-out) JSON_OUT="$2"; shift 2 ;;
    --human-out) HUMAN_OUT="$2"; shift 2 ;;
    --evidence-dir) EVIDENCE_DIR="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown arg: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ "$ROOT" = "/" ] || [ -z "$ROOT" ]; then ROOT="/"; FIXTURE=0; else ROOT="${ROOT%/}"; FIXTURE=1; fi

p() {
  if [ "$ROOT" = "/" ] || [ -z "$ROOT" ]; then printf '%s' "$1"; else printf '%s%s' "$ROOT" "$1"; fi
}

if [ "$LIVE" = "0" ]; then
  printf '%s\n' "== board_check_no_c.sh（DRY-RUN，不执行检查命令） =="
  printf 'phase=%s root=%s\n' "$PHASE" "$ROOT"
  printf '%s\n' "将只读检查 N1-N10：/proc 进程/cgroup/fd、systemctl 旧 unit、ss 端口 1234/18080、"
  printf '%s\n' "/run 旧入口、MOTU PCM owner、新栈 host 单实例与 exe/hash、wants/dependency 清理。"
  printf '%s\n' "--live 才执行；不写板端、不打开 PCM。"
  exit 0
fi

# ---- 环境前置 ---------------------------------------------------------------
ENVFAIL=0
for _cmd in bash cat grep sed tr awk date hostname readlink systemctl; do
  if ! command -v "$_cmd" >/dev/null 2>&1; then
    printf '[env] 缺少命令: %s\n' "$_cmd" >&2
    ENVFAIL=1
  fi
done
if [ "$FIXTURE" = "0" ] && [ "$(uname -s 2>/dev/null || true)" != "Linux" ]; then
  printf '[env] live 必须在 Linux 板端执行（uname=%s）\n' "$(uname -s 2>/dev/null || true)" >&2
  ENVFAIL=1
fi
if [ "$ENVFAIL" != "0" ]; then
  printf '[env] 环境不支持：exit 7\n' >&2
  exit 7
fi
HAS_SS=1
command -v ss >/dev/null 2>&1 || HAS_SS=0

# ---- 结果累积 ---------------------------------------------------------------
CHECK_IDS=(); CHECK_ST=(); CHECK_TITLE=(); CHECK_DETAIL=()
PASS_N=0; FAIL_N=0; NV_N=0; INFO_N=0
add_check() {
  CHECK_IDS+=("$1"); CHECK_ST+=("$2"); CHECK_TITLE+=("$3"); CHECK_DETAIL+=("$4")
  case "$2" in
    PASS) PASS_N=$((PASS_N+1)) ;;
    FAIL) FAIL_N=$((FAIL_N+1)) ;;
    NOT_VERIFIED) NV_N=$((NV_N+1)) ;;
    INFO) INFO_N=$((INFO_N+1)) ;;
  esac
}
pass() { add_check "$1" PASS "$2" "$3"; }
fail() { add_check "$1" FAIL "$2" "$3"; }
nv()   { add_check "$1" NOT_VERIFIED "$2" "$3"; }
info() { add_check "$1" INFO "$2" "$3"; }

json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr '\n\r\t' '   '
}
join_detail() { local IFS='; '; printf '%s' "$*"; }

# ---- 1) /proc 进程表与 fd 表（单次只读采集，N1/N3/N6/N7/N8 共用） ----------
PROC_LIST=(); FD_PID=(); FD_TGT=()
scan_procs() {
  local d pid comm args
  for d in "$(p /proc)"/[0-9]*; do
    [ -d "$d" ] || continue
    pid="${d##*/}"
    comm="$(cat "$d/comm" 2>/dev/null || true)"
    args="$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null || true)"
    PROC_LIST+=("${pid}|${comm}|${args}")
  done
}
scan_fds() {
  local fd tgt pid
  for fd in "$(p /proc)"/[0-9]*/fd/*; do
    [ -L "$fd" ] || continue
    tgt="$(readlink "$fd" 2>/dev/null || true)"
    [ -n "$tgt" ] || continue
    pid="${fd#*/proc/}"; pid="${pid%%/*}"
    FD_PID+=("$pid"); FD_TGT+=("$tgt")
  done
}
scan_procs
scan_fds

HOST_PIDS=(); DAEMON_PIDS=()
for row in "${PROC_LIST[@]}"; do
  [ -n "$row" ] || continue
  _pid="${row%%|*}"; _rest="${row#*|}"; _comm="${_rest%%|*}"; _args="${_rest#*|}"
  case "$_comm $_args" in
    *khaen-rt-host*) HOST_PIDS+=("$_pid") ;;
    *tspi-rt-daemon*) DAEMON_PIDS+=("$_pid") ;;
  esac
done
HOST_PID=""
if [ "${#HOST_PIDS[@]}" -gt 0 ]; then HOST_PID="${HOST_PIDS[0]}"; fi

old_proc_kind() {
  local pid="$1" comm="$2" args="$3"
  [ "$pid" = "$$" ] && return 0
  [ "$pid" = "$PPID" ] && return 0
  case "$comm" in grep|egrep|fgrep|pgrep|sed|awk|systemctl|ss|bash|sh) return 0 ;; esac
  case "$args" in *board_check_no_c.sh*) return 0 ;; esac
  case "$comm $args" in
    *camilladsp*) printf 'camilladsp' ;;
    *tspi-upmix*) printf 'tspi-upmix' ;;
    *khaen-audio-web*) printf 'khaen-audio-web' ;;
    *tspi-mode*) printf 'tspi-mode' ;;
    *) case " $args " in *"sox "*) printf 'sox' ;; *) return 0 ;; esac ;;
  esac
}

# ---- N1 进程 ---------------------------------------------------------------
N1_HITS=()
for row in "${PROC_LIST[@]}"; do
  _pid="${row%%|*}"; _rest="${row#*|}"; _comm="${_rest%%|*}"; _args="${_rest#*|}"
  k="$(old_proc_kind "$_pid" "$_comm" "$_args")"
  if [ -n "$k" ]; then N1_HITS+=("pid=$_pid comm=$_comm kind=$k"); fi
done
if [ "${#N1_HITS[@]}" -eq 0 ]; then
  pass N1.process "无 camilladsp/sox/旧 DSP 进程" "ps 等价 /proc 扫描命中 0（camilladsp|tspi-upmix|khaen-audio-web|tspi-mode|sox）"
else
  fail N1.process "发现旧 DSP/C 进程" "$(join_detail "${N1_HITS[@]:0:5}")"
fi

# ---- N2 旧 unit 状态 -------------------------------------------------------
OLD_UNITS="tspi-camilladsp.service tspi-upmix-autodetect.service tspi-audio.target tspi-boot-mode.service khaen-audio-web.service"
N2_BAD=(); N2_ROWS=()
for u in $OLD_UNITS; do
  if systemctl cat "$u" >/dev/null 2>&1; then
    act="$(systemctl is-active "$u" 2>/dev/null || true)"
    ena="$(systemctl is-enabled "$u" 2>/dev/null || true)"
    N2_ROWS+=("${u}[active=${act:-unknown},enabled=${ena:-unknown}]")
    case "$act" in active|activating|reloading|deactivating) N2_BAD+=("$u active=$act") ;; esac
    case "$ena" in disabled|masked|"") : ;; *) N2_BAD+=("$u enabled=$ena") ;; esac
  else
    N2_ROWS+=("${u}[not-found]")
  fi
done
if [ "${#N2_BAD[@]}" -eq 0 ]; then
  pass N2.units "旧音频 unit 不存在或 disabled/masked 且 inactive" "$(join_detail "${N2_ROWS[@]}")"
else
  fail N2.units "旧 C/旧入口 unit 仍 active 或 enabled" "$(join_detail "${N2_BAD[@]}")"
fi

# ---- N3 cgroup -------------------------------------------------------------
N3_HITS=()
CGROOT="$(p /sys/fs/cgroup)"
if [ -d "$CGROOT" ]; then
  for d in "$CGROOT"/system.slice/*; do
    [ -e "$d" ] || continue
    case "${d##*/}" in *camilladsp*|*tspi-upmix*|*khaen-audio-web*) N3_HITS+=("slice=${d##*/}") ;; esac
  done
fi
for d in "$(p /proc)"/[0-9]*; do
  [ -r "$d/cgroup" ] || continue
  cg="$(cat "$d/cgroup" 2>/dev/null || true)"
  case "$cg" in
    *camilladsp*|*tspi-upmix*|*khaen-audio-web*) N3_HITS+=("pid=${d##*/} cgroup=$cg") ;;
  esac
done
if [ "${#N3_HITS[@]}" -eq 0 ]; then
  pass N3.cgroup "无进程位于旧 C unit cgroup" "/proc/*/cgroup 与 system.slice 均无 camilladsp/tspi-upmix/khaen-audio-web"
else
  fail N3.cgroup "仍有进程在旧 C unit cgroup" "$(join_detail "${N3_HITS[@]:0:5}")"
fi

# ---- N4 旧控制端口 1234/18080 ---------------------------------------------
N4_BAD=(); LISTEN_ALL=""; N4_SKIPPED=0
if [ "$HAS_SS" = "1" ]; then
  LISTEN_ALL="$(ss -lntup 2>/dev/null || true)"
  EST_ALL="$(ss -tnp 2>/dev/null || true)"
  for port in 1234 18080; do
    hit="$(printf '%s\n' "$LISTEN_ALL" | grep -E "[:.]$port([^0-9]|$)" | head -n 3 || true)"
    if [ -n "$hit" ]; then N4_BAD+=("LISTEN :$port -> $hit"); fi
    hit="$(printf '%s\n' "${EST_ALL:-}" | grep -E "[:.]$port([^0-9]|$)" | head -n 3 || true)"
    if [ -n "$hit" ]; then N4_BAD+=("CONN :$port -> $hit"); fi
  done
else
  nv N4.ports "旧控制端口未检测" "ss 不可用：无法验证 1234/18080 无监听"
  N4_SKIPPED=1
fi
if [ "$N4_SKIPPED" != "1" ]; then
  if [ "${#N4_BAD[@]}" -eq 0 ]; then
    pass N4.ports "旧控制端口 1234/18080 无监听/无连接" "ss -lntup/-tnp 命中 0"
  else
    fail N4.ports "旧控制端口仍在使用" "$(join_detail "${N4_BAD[@]:0:5}")"
  fi
fi

# ---- N5 旧 C 激活入口文件 --------------------------------------------------
N5_BAD=()
for f in /run/tspi/camilladsp.yml /run/tspi/camilladsp.env /etc/tspi/last-mode /run/tspi/mode; do
  if [ -e "$(p "$f")" ]; then N5_BAD+=("$f"); fi
done
if [ "${#N5_BAD[@]}" -eq 0 ]; then
  pass N5.entry-files "旧 C 激活入口文件已清理" "/run/tspi/camilladsp.{yml,env} /etc/tspi/last-mode /run/tspi/mode 均不存在"
else
  fail N5.entry-files "旧 C 激活入口仍存在" "$(join_detail "${N5_BAD[@]}")"
fi

# ---- N6 socket/pipe 依赖 ---------------------------------------------------
N6_BAD=()
for f in "$(p /run)"/camilladsp* "$(p /run)/tspi-camilladsp"* "$(p /run/tspi)"/camilladsp* "$(p /run/tspi)"/tspi-upmix*; do
  if [ -e "$f" ]; then N6_BAD+=("path=$f"); fi
done
if [ "$HAS_SS" = "1" ]; then
  hit="$(ss -xnp 2>/dev/null | grep -E 'camilladsp|tspi-upmix' | head -n 3 || true)"
  if [ -n "$hit" ]; then N6_BAD+=("unix-socket=$hit"); fi
fi
for i in "${!FD_PID[@]}"; do
  pidl="${FD_PID[${i}]}"; tgt="${FD_TGT[${i}]}"
  case "$tgt" in
    *camilladsp*|*tspi-upmix*) N6_BAD+=("fd pid=$pidl -> $tgt") ;;
    */run/tspi/*) N6_BAD+=("fd pid=$pidl -> $tgt(旧 run 目录)") ;;
  esac
done
if [ "${#N6_BAD[@]}" -eq 0 ]; then
  pass N6.socket-pipe "无对 C 的 socket/pipe 依赖" "旧 socket/FIFO 路径 0；fd 无 camilladsp/tspi-upmix/旧 /run/tspi 目标"
else
  fail N6.socket-pipe "存在对 C 的 socket/pipe 依赖" "$(join_detail "${N6_BAD[@]:0:5}")"
fi

# ---- N7 MOTU PCM 唯一 owner ------------------------------------------------
MOTU_CARD=""
CARDS="$(cat "$(p /proc/asound/cards)" 2>/dev/null || true)"
MOTU_LINE="$(printf '%s\n' "$CARDS" | grep -i 'UltraLite' | head -n 1 || true)"
if [ -n "$MOTU_LINE" ]; then
  MOTU_CARD="$(printf '%s\n' "$MOTU_LINE" | awk '{print $1}' | tr -dc '0-9')"
fi
N7_BAD=(); N7_ROWS=()
if [ -z "$MOTU_CARD" ]; then
  if [ "$ALLOW_NO_MOTU" = "1" ]; then
    nv N7.pcm-owner "MOTU 未枚举，PCM owner 无法验证" "--allow-no-motu 已放宽；T72 正式口径必须要求 MOTU"
  else
    fail N7.pcm-owner "MOTU 未枚举" "/proc/asound/cards 无 UltraLite（无法证明 host 是唯一 PCM owner）"
  fi
else
  for i in "${!FD_PID[@]}"; do
    pidl="${FD_PID[${i}]}"; tgt="${FD_TGT[${i}]}"
    case "$tgt" in
      */dev/snd/pcmC${MOTU_CARD}D*|/dev/snd/pcmC${MOTU_CARD}D*|*/dev/snd/controlC${MOTU_CARD}|/dev/snd/controlC${MOTU_CARD}|*/dev/snd/hwC${MOTU_CARD}D*|*/by-id/usb-MOTU*)
        if [ -n "$HOST_PID" ] && [ "$pidl" = "$HOST_PID" ]; then
          N7_ROWS+=("owner=host($pidl) $tgt")
        else
          N7_BAD+=("pid=$pidl $tgt (非 host)")
        fi ;;
    esac
  done
  for f in "$(p /proc)"/asound/card${MOTU_CARD}/pcm*/sub*/status; do
    [ -r "$f" ] || continue
    st="$(cat "$f" 2>/dev/null || true)"
    owner="$(printf '%s\n' "$st" | awk -F: '/owner_pid/ {gsub(/ /,"",$2); print $2; exit}')"
    state="$(printf '%s\n' "$st" | awk -F: '/^state/ {gsub(/ /,"",$2); print $2; exit}')"
    if [ -n "$owner" ] && [ "$owner" != "0" ]; then
      if [ -n "$HOST_PID" ] && [ "$owner" = "$HOST_PID" ]; then
        N7_ROWS+=("pcm-owner=host state=${state:-?}")
      else
        N7_BAD+=("pcm owner=$owner state=${state:-?} (非 host=$HOST_PID) $f")
      fi
    fi
  done
  if [ "${#N7_BAD[@]}" -eq 0 ]; then
    pass N7.pcm-owner "新 host 为 MOTU PCM 唯一 owner" "card=$MOTU_CARD 持有者/owner_pid 仅 host=${HOST_PID:-none}；采集 ${#N7_ROWS[@]} 条"
  else
    fail N7.pcm-owner "MOTU PCM 存在非 host owner" "$(join_detail "${N7_BAD[@]:0:5}")"
  fi
fi

# ---- N8 新栈 host/daemon 单实例 + exe/hash --------------------------------
HOST_EXE=""; HOST_SHA=""; DAEMON_EXE=""; DAEMON_SHA=""; DAEMON_PID=""
if [ "${#HOST_PIDS[@]}" -eq 0 ]; then
  if [ "$ALLOW_HOST_INACTIVE" = "1" ]; then
    nv N8.host "新 host 未运行（已放宽）" "回滚/mute 阶段快照；T72 播放/校准验收时必须 active"
  else
    fail N8.host "khaen-rt-host 未运行" "未找到 khaen-rt-host 进程（phase=$PHASE）"
  fi
elif [ "${#HOST_PIDS[@]}" -gt 1 ]; then
  fail N8.host "khaen-rt-host 多实例" "pids=$(join_detail "${HOST_PIDS[@]}")（双实例=双 PCM owner 风险）"
else
  HOST_PID="${HOST_PIDS[0]}"
  HOST_EXE="$(readlink "$(p /proc)/$HOST_PID/exe" 2>/dev/null || true)"
  HOST_EXE="${HOST_EXE% (deleted)}"
  _real="${HOST_EXE}"
  if [ ! -e "${_real}" ]; then _real="$(p "$HOST_EXE")"; fi
  if [ -e "${_real}" ] && command -v sha256sum >/dev/null 2>&1; then
    HOST_SHA="$(sha256sum "${_real}" 2>/dev/null | awk '{print $1}' || true)"
  fi
  _unit_act="$(systemctl is-active khaen-rt-host.service 2>/dev/null || true)"
  _exe_ok=0
  case "$HOST_EXE" in */opt/tspi-rt-stack/*) _exe_ok=1 ;; esac
  if [ "${_exe_ok}" = "1" ] && [ "${_unit_act:-}" = "active" ]; then
    pass N8.host "新 host 单实例 active 且为部署路径" "pid=$HOST_PID unit=active exe=$HOST_EXE sha256=${HOST_SHA:-unavailable}"
  else
    fail N8.host "新 host 可执行/unit 状态不符" "pid=$HOST_PID unit=${_unit_act:-unknown} exe=$HOST_EXE（要求 active 且 /opt/tspi-rt-stack）"
  fi
fi
if [ "${#DAEMON_PIDS[@]}" -gt 0 ]; then
  DAEMON_PID="${DAEMON_PIDS[0]}"
  DAEMON_EXE="$(readlink "$(p /proc)/$DAEMON_PID/exe" 2>/dev/null || true)"
  DAEMON_EXE="${DAEMON_EXE% (deleted)}"
  _real2="${DAEMON_EXE}"
  if [ ! -e "${_real2}" ]; then _real2="$(p "$DAEMON_EXE")"; fi
  if [ -e "${_real2}" ] && command -v sha256sum >/dev/null 2>&1; then
    DAEMON_SHA="$(sha256sum "${_real2}" 2>/dev/null | awk '{print $1}' || true)"
  fi
  if [ "$REQUIRE_DAEMON" = "1" ]; then
    _dact="$(systemctl is-active tspi-rt-daemon.service 2>/dev/null || true)"
    if [ "${_dact:-}" = "active" ]; then
      info N8.daemon "daemon active（--require-daemon）" "pid=$DAEMON_PID exe=$DAEMON_EXE sha256=${DAEMON_SHA:-unavailable}"
    else
      fail N8.daemon "daemon 未 active（--require-daemon）" "unit=${_dact:-unknown} pid=$DAEMON_PID"
    fi
  else
    info N8.daemon "daemon 运行中（可选门）" "pid=$DAEMON_PID exe=$DAEMON_EXE sha256=${DAEMON_SHA:-unavailable}"
  fi
else
  if [ "$REQUIRE_DAEMON" = "1" ]; then
    fail N8.daemon "daemon 未运行（--require-daemon）" "未找到 tspi-rt-daemon 进程"
  else
    info N8.daemon "daemon 未运行（可选门，未要求）" "未找到 tspi-rt-daemon 进程；--require-daemon 时为硬门"
  fi
fi
_tgt_act="$(systemctl is-active tspi-rt-stack.target 2>/dev/null || true)"
info N8.target "新栈 target 状态" "tspi-rt-stack.target active=${_tgt_act:-unknown}"

# ---- N9 依赖/激活入口清理 --------------------------------------------------
N9_BAD=()
for d in "$(p /etc/systemd/system)"/*.wants "$(p /etc/systemd/system)"/*.requires; do
  [ -d "$d" ] || continue
  for l in "$d"/*; do
    if [ -e "$l" ] || [ -L "$l" ]; then
      bn="${l##*/}"
      case "$bn" in
        tspi-camilladsp.service|tspi-upmix-autodetect.service|khaen-audio-web.service|tspi-audio.target|tspi-boot-mode.service)
          N9_BAD+=("activation-symlink=$l") ;;
      esac
    fi
  done
done
for f in "$(p /etc/systemd/system)"/*camilladsp*.socket "$(p /etc/systemd/system)"/*camilladsp*.path "$(p /etc/systemd/system)"/*tspi-audio*.socket; do
  if [ -e "$f" ]; then N9_BAD+=("socket/path-unit=$f"); fi
done
NEW_UNITS="khaen-rt-host.service tspi-rt-daemon.service tspi-rt-stack.target tspi-motu-48k-keeper.service"
for u in $NEW_UNITS; do
  content="$(systemctl cat "$u" 2>/dev/null || true)"
  [ -n "$content" ] || continue
  while IFS= read -r line; do
    case "$line" in
      '#'*) continue ;;
      *camilladsp*|*tspi-upmix*|*tspi-audio.target*|*khaen-audio-web*|*tspi-boot-mode*)
        case "$line" in
          ConditionPathExists=!*|AssertPathExists=!*) continue ;;
          Before=*|After=*) N9_BAD+=("$u ordering-only: $line") ;;
          *) N9_BAD+=("$u positive-dep: $line") ;;
        esac ;;
    esac
  done <<< "$content"
done
if [ "${#N9_BAD[@]}" -eq 0 ]; then
  pass N9.deps "旧 C 依赖/激活入口已清理" "无 *.wants 指向旧 unit；新栈 unit 无旧 C 正向/排序依赖（负向守卫允许）"
else
  fail N9.deps "存在旧 C 依赖或激活入口" "$(join_detail "${N9_BAD[@]:0:6}")"
fi

# ---- N10 历史文件（INFO，不 FAIL） ----------------------------------------
C_BIN="$(p /usr/local/bin/camilladsp)"
if [ -f "$C_BIN" ]; then
  _csha=""
  if command -v sha256sum >/dev/null 2>&1; then _csha="$(sha256sum "$C_BIN" 2>/dev/null | awk '{print $1}' || true)"; fi
  info N10.historical "历史 C 二进制存在（不 FAIL）" "path=/usr/local/bin/camilladsp size=$(stat -c '%s' "$C_BIN" 2>/dev/null || echo '?') sha256=${_csha:-unavailable}"
else
  info N10.historical "历史 C 二进制不存在" "/usr/local/bin/camilladsp 不存在（允许：已清理）"
fi
if [ -d "$(p /etc/camilladsp)" ]; then
  info N10.historical-config "历史 C 配置目录存在（不 FAIL）" "/etc/camilladsp（DEC §7：历史配置保留 != 旧服务运行）"
fi

# ---- 保留 AoIP/PTP unit（T72 播放/校准阶段可能 active，仅记录） -----------
RETAIN_UNITS="tspi-statime-inferno.service tspi-aes67-daemon.service tspi-ravenna-lkm.service tspi-ptp4l-master.service tspi-phc2sys.service"
RETAIN_ROWS=(); RETAIN_ACTIVE=()
for u in $RETAIN_UNITS; do
  _a="$(systemctl is-active "$u" 2>/dev/null || true)"
  RETAIN_ROWS+=("${u}=${_a:-unknown}")
  case "$_a" in active|activating) RETAIN_ACTIVE+=("$u") ;; esac
done
if [ "${#RETAIN_ACTIVE[@]}" -eq 0 ]; then
  info N10.retained "保留 AoIP/PTP unit inactive" "$(join_detail "${RETAIN_ROWS[@]}")"
elif [ "$REQUIRE_AUDIO_OFF" = "1" ]; then
  fail N10.retained "保留 AoIP/PTP unit 在 audio-off 阶段仍 active" "$(join_detail "${RETAIN_ACTIVE[@]}")"
else
  info N10.retained "保留 AoIP/PTP unit 有 active（播放模式正常）" "$(join_detail "${RETAIN_ROWS[@]}")"
fi

# ---- 输出 JSON / 人类可读 --------------------------------------------------
HOSTNAME_V="$(hostname 2>/dev/null || echo unknown)"
UNAME_V="$(uname -a 2>/dev/null || echo unknown)"
BOOT_ID="$(cat "$(p /proc/sys/kernel/random/boot_id)" 2>/dev/null || echo unknown)"
UPTIME_V="$(cut -d' ' -f1 < "$(p /proc/uptime)" 2>/dev/null || echo unknown)"
GENERATED="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
if [ "$FAIL_N" -gt 0 ]; then OVERALL="FAIL"; elif [ "$NV_N" -gt 0 ]; then OVERALL="INCOMPLETE"; else OVERALL="PASS"; fi

build_json() {
  local i sep idx
  printf '{\n'
  printf ' "schema": "tspi-rt-stack/no-c-proof-board/v1",\n'
  printf ' "task": "T72",\n'
  printf ' "generator": "%s",\n' "$VERSION"
  printf ' "phase": "%s",\n' "$(json_escape "$PHASE")"
  if [ "$FIXTURE" = "1" ]; then printf ' "mode": "fixture",\n'; else printf ' "mode": "live",\n'; fi
  printf ' "root": "%s",\n' "$(json_escape "$ROOT")"
  printf ' "generated_at": "%s",\n' "$GENERATED"
  printf ' "host": {"hostname": "%s", "uname": "%s", "boot_id": "%s", "uptime_s": "%s"},\n' \
    "$(json_escape "$HOSTNAME_V")" "$(json_escape "$UNAME_V")" "$(json_escape "$BOOT_ID")" "$(json_escape "$UPTIME_V")"
  printf ' "overall": "%s",\n' "$OVERALL"
  printf ' "counts": {"pass": %d, "fail": %d, "not_verified": %d, "info": %d},\n' "$PASS_N" "$FAIL_N" "$NV_N" "$INFO_N"
  printf ' "inventory": {\n'
  printf '  "host_pid": "%s",\n' "$(json_escape "$HOST_PID")"
  printf '  "host_exe": "%s",\n' "$(json_escape "$HOST_EXE")"
  printf '  "host_sha256": "%s",\n' "$(json_escape "$HOST_SHA")"
  printf '  "daemon_pid": "%s",\n' "$(json_escape "$DAEMON_PID")"
  printf '  "daemon_exe": "%s",\n' "$(json_escape "$DAEMON_EXE")"
  printf '  "daemon_sha256": "%s",\n' "$(json_escape "$DAEMON_SHA")"
  printf '  "motu_card": "%s",\n' "$(json_escape "$MOTU_CARD")"
  printf '  "old_units": ['
  sep=""; for r in "${N2_ROWS[@]}"; do printf '%s"%s"' "$sep" "$(json_escape "$r")"; sep=", "; done
  printf '],\n'
  printf '  "retained_units": ['
  sep=""; for r in "${RETAIN_ROWS[@]}"; do printf '%s"%s"' "$sep" "$(json_escape "$r")"; sep=", "; done
  printf '],\n'
  printf '  "listeners": ['
  sep=""; while IFS= read -r l; do [ -n "$l" ] || continue; printf '%s"%s"' "$sep" "$(json_escape "$l")"; sep=", "; done <<< "$LISTEN_ALL"
  printf ']\n'
  printf ' },\n'
  printf ' "checks": [\n'
  i=0
  for idx in "${!CHECK_IDS[@]}"; do
    i=$((i+1))
    printf '  {"id": "%s", "status": "%s", "title": "%s", "detail": "%s"}' \
      "$(json_escape "${CHECK_IDS[${idx}]}")" "$(json_escape "${CHECK_ST[${idx}]}")" \
      "$(json_escape "${CHECK_TITLE[${idx}]}")" "$(json_escape "${CHECK_DETAIL[${idx}]}")"
    if [ "$i" -lt "${#CHECK_IDS[@]}" ]; then printf ','; fi
    printf '\n'
  done
  printf ' ]\n}\n'
}

build_human() {
  local idx
  printf '%s\n' "== T72 no-C 板端检查（只读）phase=$PHASE =="
  printf '%s\n' "host=$HOSTNAME_V boot_id=$BOOT_ID uptime_s=$UPTIME_V generated_at=$GENERATED"
  printf '%s\n' "root=$ROOT host_pid=$HOST_PID host_exe=$HOST_EXE host_sha256=${HOST_SHA:-unavailable}"
  printf '%s\n' "motu_card=${MOTU_CARD:-none}"
  printf '%s\n' "old_units=$(join_detail "${N2_ROWS[@]}")"
  printf '\n'
  for idx in "${!CHECK_IDS[@]}"; do
    printf '[%s] %-18s %s\n' "${CHECK_ST[${idx}]}" "${CHECK_IDS[${idx}]}" "${CHECK_TITLE[${idx}]}"
    printf '       %s\n' "${CHECK_DETAIL[${idx}]}"
  done
  printf '\n== 结论: overall=%s pass=%d fail=%d not_verified=%d info=%d ==\n' "$OVERALL" "$PASS_N" "$FAIL_N" "$NV_N" "$INFO_N"
  if [ "$FAIL_N" -gt 0 ]; then
    printf '%s\n' "判定：G-no-C 运行时 FAIL。T72 不得签收；若处于 T71 窗口内，只允许 08-rollback.sh（last-good/mute），绝不启动/启用旧 C。"
  fi
}

if [ -n "$EVIDENCE_DIR" ]; then
  mkdir -p "$EVIDENCE_DIR" 2>/dev/null || true
  if [ -z "$JSON_OUT" ]; then JSON_OUT="$EVIDENCE_DIR/board-no-c-check.json"; fi
  if [ -z "$HUMAN_OUT" ]; then HUMAN_OUT="$EVIDENCE_DIR/board-no-c-check.txt"; fi
fi
if [ -n "$JSON_OUT" ]; then
  build_json > "$JSON_OUT"
else
  build_json
fi
if [ -n "$HUMAN_OUT" ]; then
  build_human > "$HUMAN_OUT"
fi
build_human
if [ -n "$EVIDENCE_DIR" ]; then
  {
    printf '%s\n' "# commands executed (read-only)"
    printf 'generated_at=%s version=%s phase=%s root=%s\n' "$GENERATED" "$VERSION" "$PHASE" "$ROOT"
    printf '%s\n' "# 只读：/proc/{comm,cmdline,cgroup,fd/*,exe,asound}、/sys/fs/cgroup、/etc/systemd/system、/run；systemctl cat/is-active/is-enabled；ss -lntup/-tnp/-xnp"
    printf '%s\n' "bash board_check_no_c.sh --live --root $ROOT --phase $PHASE with require_daemon=$REQUIRE_DAEMON"
  } > "$EVIDENCE_DIR/commands.log" 2>/dev/null || true
fi

if [ "$FAIL_N" -gt 0 ]; then exit 5; fi
exit 0
