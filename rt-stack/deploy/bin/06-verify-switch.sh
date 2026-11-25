#!/usr/bin/env bash
# T28 06-verify-switch.sh —— 切换后验证（只读；写 verify-report.json）
#
# 硬门（任一 FAIL -> exit 5）：
#   V1 khaen-rt-host.service active（--require-daemon 时 daemon 也 active）
#   V2 旧 C 退役保持：C unit 无 enabled/active、入口文件不存在、无 camilladsp/khaen-upmix 进程
#   V3 保留 AoIP/PTP unit inactive（audio-off；--allow-retained-active 可放宽并记 WARN）
#   V4 新 host 进程存活；/dev/snd 仅由新 host 持有（无其它音频 owner）
#   V5 旧控制面端口：1234 无监听；80 若被监听不得属于旧 web/camilladsp（新 daemon 监听 80 是合法的）
#   V6 进程在 --settle-secs 后仍存活（无启动即崩）
#   V10 IRQ 亲和持久化（task-192）：irqbalance 未 enabled/active；khaen-irq-affinity.service active；
#       IRQ58/56/57 effective 仍在 A53（1/2/3）且 T45 IRQ 放置前置 PASS（host 启动后复验）
#       B1（2026-11-20 板窗）：V10.bootwait 已装 oneshot 含 ExecStartPre=irq-affinity-pin wait；
#       V10.legacy-dropin 无遗留 live-only drop-in（开机 IRQ56/57 晚到 → host 不启动的回归门）
# 软门（记 NOT_WIRED/NOT_VERIFIED，不改退出码，除非对应 --require-*）：
#   V7 RT 属性（limits 的 rtprio/memlock；--require-rt 时硬门）
#   V8 健康端点（--require-health 时硬门；默认只有 URL 给定时探测）
#
# 用法: ./06-verify-switch.sh --live [--timeout 60] [--require-daemon] [--require-rt]
#        [--require-health --health-url http://127.0.0.1:8090/healthz] [--root DIR]
# 退出码: 0 硬门全 PASS（含软门 NOT_WIRED）；2 用法；5 有硬门 FAIL。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

TIMEOUT="${KHAEN_TIMEOUT_SECS:-120}"
REQUIRE_DAEMON=0
REQUIRE_RT=0
REQUIRE_HEALTH=0
HEALTH_URL="${HEALTH_URL:-http://127.0.0.1:8090/healthz}"
ALLOW_RETAINED=0
SETTLE_SECS=3
usage() { sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --evidence) EVIDENCE_DIR="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --settle-secs) SETTLE_SECS="$2"; shift 2 ;;
    --require-daemon) REQUIRE_DAEMON=1; shift ;;
    --require-rt) REQUIRE_RT=1; shift ;;
    --require-health) REQUIRE_HEALTH=1; shift ;;
    --health-url) HEALTH_URL="$2"; shift 2 ;;
    --allow-retained-active) ALLOW_RETAINED=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 将验证 V1-V10：new host/daemon active + /etc toml、C retired（masked）、retained inactive、/dev/snd 独占、端口 1234/80、进程稳定、RT 属性、health、MOTU 48k keeper、IRQ 亲和持久化（irqbalance 未复活 + IRQ 仍在 A53 + T45 IRQ 前置 + B1 开机等待前置/无遗留 drop-in）"
  exit 0
fi

evidence_prepare "06-verify-switch"
FAILS=0
SOFT=0
checks=""
esc() { local s="${1//\\/\\\\}"; s="${s//\"/\\\"}"; printf "%s" "$s" | tr "\n" " "; }
add_check() { # id status detail
  local id esc_id esc_st esc_de
  esc_id="$(esc "$1")"; esc_st="$(esc "$2")"; esc_de="$(esc "$3")"
  checks="$checks{\"id\":\"$esc_id\",\"status\":\"$esc_st\",\"detail\":\"$esc_de\"},"
  case "$2" in FAIL) FAILS=$((FAILS+1));; NOT_WIRED|NOT_VERIFIED) SOFT=$((SOFT+1));; esac
  log "[$2] $1: $3"
}

log "== 06 verify-switch（timeout=${TIMEOUT}s settle=${SETTLE_SECS}s）"

# V1 units + /etc 配置（F-18/T28-P0-2：配置缺失时 unit 启动即退，必须硬门）
if wait_unit_active khaen-rt-host.service "$TIMEOUT"; then
  add_check V1.unit.host PASS "khaen-rt-host.service active"
else
  add_check V1.unit.host FAIL "khaen-rt-host.service ${TIMEOUT}s 内未 active"
fi
MISSING_CFG=""
for f in rt-host.toml rt-daemon.toml; do
  [ -f "$(p "$KHAEN_ETC/$f")" ] || MISSING_CFG="$MISSING_CFG $KHAEN_ETC/$f"
done
if [ -z "$MISSING_CFG" ]; then add_check V1.config PASS "/etc/khaen-rt-stack/{rt-host,rt-daemon}.toml 已安装"
else add_check V1.config FAIL "缺新栈配置文件（04 未安装 toml）:$MISSING_CFG"; fi
if [ "$REQUIRE_DAEMON" = "1" ]; then
  if wait_unit_active khaen-rt-daemon.service "$TIMEOUT"; then add_check V1.unit.daemon PASS "khaen-rt-daemon.service active"; else add_check V1.unit.daemon FAIL "daemon 未 active"; fi
else
  if sunit_active khaen-rt-daemon.service; then add_check V1.unit.daemon PASS "daemon active"; else add_check V1.unit.daemon NOT_VERIFIED "daemon inactive（本批 NOT_BUILT；--require-daemon 才是硬门）"; fi
fi

# V2 C retired
BAD="$(assert_c_retired || true)"
if [ -z "$(printf '%s' "$BAD" | tr -d '[:space:]')" ]; then
  add_check V2.c-retired PASS "无 C unit enabled/active、无 C 进程"
else
  add_check V2.c-retired FAIL "旧 C 未退役: $(printf '%s' "$BAD" | tr '\n' ' ')"
fi
ENTRY=""
for f in "${C_ENTRY_FILES[@]}"; do [ -e "$(p "$f")" ] && ENTRY="$ENTRY $f"; done
[ -z "$ENTRY" ] && add_check V2.c-entry PASS "旧 C 激活入口不存在" || add_check V2.c-entry FAIL "旧 C 激活入口仍存在:$ENTRY"

# V3 retained AoIP/PTP inactive
BUSY=""
for u in "${RETAIN_AOIP_UNITS[@]}"; do sunit_active "$u" && BUSY="$BUSY $u"; done
if [ -z "$BUSY" ]; then add_check V3.retained PASS "保留 AoIP/PTP unit inactive（audio-off）"
elif [ "$ALLOW_RETAINED" = "1" ]; then add_check V3.retained NOT_VERIFIED "保留 unit active（--allow-retained-active）:$BUSY"
else add_check V3.retained FAIL "保留 AoIP/PTP unit 仍 active（设备独占冲突）:$BUSY"; fi

# V4 新 host 进程与设备独占
HOST_PID="$(pgrep -f 'khaen-rt-host' 2>/dev/null | head -n 1 || true)"
if [ -n "$HOST_PID" ]; then add_check V4.proc.alive PASS "khaen-rt-host pid=$HOST_PID"; else add_check V4.proc.alive FAIL "未找到 khaen-rt-host 进程"; fi
# /dev/snd holders（排除新 host pid）
HOLDERS=""
SND_DIR="$(p /dev/snd)"
for fd in "$(p /proc)"/[0-9]*/fd/*; do
  [ -L "$fd" ] || continue
  tgt="$(readlink "$fd" 2>/dev/null || true)"
  case "$tgt" in
    /dev/snd/*)
      pid="$(printf '%s' "$fd" | sed -E 's#.*/proc/([0-9]+)/fd/.*#\1#')"
      [ -n "$HOST_PID" ] && [ "$pid" = "$HOST_PID" ] && continue
      HOLDERS="$HOLDERS pid=$pid:$tgt"
      ;;
  esac
done 2>/dev/null || true
if [ -z "$HOLDERS" ]; then add_check V4.device.exclusive PASS "/dev/snd 无其它持有者（新 host 例外）"; else add_check V4.device.exclusive FAIL "/dev/snd 被其它进程持有:$HOLDERS"; fi

# V5 old ports（F-42：1234 + 80；80 允许新 daemon 监听，但不得是旧 web/camilladsp）
LISTEN="$(ss -ltnp 2>/dev/null | grep -E ':(1234)\\b' || true)"
if [ -z "$LISTEN" ]; then add_check V5.ports.1234 PASS "旧 Camilla WS 端口 1234 无监听"
else add_check V5.ports.1234 FAIL "1234 仍有监听: $LISTEN"; fi
L80="$(ss -ltnp 2>/dev/null | grep -E ':80\\b' || true)"
if [ -z "$L80" ]; then
  add_check V5.ports.80 PASS "80 无监听"
elif printf '%s' "$L80" | grep -Eqi 'khaen-audio-web|camilladsp|khaen-upmix'; then
  add_check V5.ports.80 FAIL "80 仍被旧控制面进程监听: $L80"
else
  add_check V5.ports.80 PASS "80 由新 daemon/系统进程监听（非旧 C）: $(printf '%s' "$L80" | head -n 1)"
fi

# V6 settle
START_PID="$HOST_PID"
sleep "$SETTLE_SECS" 2>/dev/null || true
if [ -n "$START_PID" ] && [ -d "$(p /proc)/$START_PID" ]; then
  add_check V6.settle PASS "${SETTLE_SECS}s 后进程仍存活"
else
  add_check V6.settle FAIL "进程在 ${SETTLE_SECS}s 内退出"
fi

# V9 MOTU 48k keeper（D8/用户禁止 192k；boot+timer+udev 兜底）
if sunit_exists khaen-motu-48k-keeper.timer; then
  KSTATE="$(sunit_enabled_state khaen-motu-48k-keeper.timer)"
  case "$KSTATE" in
    enabled|enabled-runtime) add_check V9.keeper.timer PASS "khaen-motu-48k-keeper.timer enabled（boot/timer/udev 兜底）" ;;
    *) add_check V9.keeper.timer FAIL "keeper timer 未 enabled（state=${KSTATE:-unknown}）" ;;
  esac
else
  add_check V9.keeper.timer FAIL "khaen-motu-48k-keeper.timer 未安装"
fi
if [ -x "$(p /usr/local/lib/khaen/motu-48k-keeper)" ]; then
  add_check V9.keeper.script PASS "/usr/local/lib/khaen/motu-48k-keeper 可执行"
else
  add_check V9.keeper.script FAIL "缺少 48k keeper 脚本"
fi
# keeper 必须"确实在跑"，不是"装了没跑"（2026-11-20 实况：unit inactive/dead + 日志不存在，
# 当天 06 只在 V9.keeper.run 给了 NOT_VERIFIED，等于防线形同虚设）
KPERIOD="$(systemctl show -p OnUnitActiveSec --value khaen-motu-48k-keeper.timer 2>/dev/null || true)"
case "$KPERIOD" in
  ''|infinity) add_check V9.keeper.period FAIL "keeper timer 无周期（OnUnitActiveSec 空/infinity）" ;;
  *) if [ "${KPERIOD%min}" != "$KPERIOD" ]; then KP=9999; else KP="${KPERIOD%s}"; fi
     if [ "$KP" -le 120 ] 2>/dev/null; then add_check V9.keeper.period PASS "keeper 周期 ${KPERIOD}（<=120s）"
     else add_check V9.keeper.period FAIL "keeper 周期 ${KPERIOD} 过长（要求 <=120s，192k 漂移窗口靠它兜）"; fi ;;
esac
KLOGF="$(p /var/log/khaen/motu-48k-keeper.log)"
KJ="$(journalctl -t khaen-motu-48k-keeper --since '-5 min' --no-pager 2>/dev/null | grep -c 'stageB cuemix\|stageA\|done' || true)"
KF="$( [ -f "$KLOGF" ] && find "$KLOGF" -mmin -5 2>/dev/null | wc -l || echo 0 )"
if [ "${KJ:-0}" -ge 1 ] || [ "${KF:-0}" -ge 1 ]; then
  add_check V9.keeper.run PASS "近 5min 有 keeper 活动记录（journal=${KJ:-0} file=${KF:-0}）"
else
  add_check V9.keeper.run FAIL "近 5min 无任何 keeper 活动（journal=${KJ:-0} file=${KF:-0}）→ 48k 防线未生效，先 systemctl start khaen-motu-48k-keeper.service 再看日志"
fi
KRATE="$(journalctl -t khaen-motu-48k-keeper --since '-10 min' --no-pager 2>/dev/null | grep -o 'device_rate=[0-9]*' | tail -n 1 | cut -d= -f2)"
[ -z "$KRATE" ] && [ -f "$KLOGF" ] && KRATE="$(grep -o 'device_rate=[0-9]*' "$KLOGF" 2>/dev/null | tail -n 1 | cut -d= -f2)"
case "$KRATE" in
  48000) add_check V9.device.rate PASS "CueMix 报设备速率 48000" ;;
  '') add_check V9.device.rate NOT_VERIFIED "未取到 CueMix device_rate（keeper 未到 stageB 或客户端缺失）→ 用 runbook 只读命令人工核 kSampleRate" ;;
  *) add_check V9.device.rate FAIL "CueMix 报设备速率 $KRATE != 48000（192k/96k 漂移！光口在此速率下不可用）" ;;
esac

# V11 板级韧性（2026-11-21）：watchdog / panic-on-lockup / pstore 归档 / 温度日志
WD_US="$(systemctl show -p RuntimeWatchdogUSec --value 2>/dev/null | tr -d ' ')"
case "$WD_US" in
  ''|0|0s|infinity) add_check V11.watchdog FAIL "systemd RuntimeWatchdogSec 未生效（RuntimeWatchdogUSec=${WD_US:-empty}）→ 挂死就要等人拔电；查 /dev/watchdog 是否存在 + system.conf.d/10-khaen-watchdog.conf" ;;
  *) add_check V11.watchdog PASS "systemd 看门狗生效（RuntimeWatchdogUSec=$WD_US）" ;;
esac
POO="$(cat /proc/sys/kernel/panic_on_oops 2>/dev/null)"
HLP="$(cat /proc/sys/kernel/hardlockup_panic 2>/dev/null)"
if [ "${POO:-0}" = "1" ] && [ "${HLP:-0}" = "1" ]; then
  add_check V11.panic PASS "panic_on_oops=1 / hardlockup_panic=1（锁死会留下 ramoops 并自动重启）"
else
  add_check V11.panic FAIL "panic_on_oops=${POO:-?} hardlockup_panic=${HLP:-?}（要求都为 1；否则挂死时什么都不留）"
fi
PARCH=""
[ -f "$(p /var/log/khaen/pstore)/INDEX.txt" ] && PARCH="$(ls -d "$(p /var/log/khaen/pstore)"/*/ 2>/dev/null | wc -l)"
if [ -x "$(p /usr/local/lib/khaen/pstore-archive)" ]; then
  if sunit_exists khaen-pstore-archive.service; then
    add_check V11.pstore PASS "pstore 归档已安装（已归档目录数=${PARCH:-0}；空的也要有 INDEX.txt 记录）"
  else
    add_check V11.pstore FAIL "pstore-archive 脚本在但 unit 未安装"
  fi
else
  add_check V11.pstore FAIL "缺 /usr/local/lib/khaen/pstore-archive（崩溃证据无法落盘）"
fi
TLOG="$(p /var/log/khaen/thermal.log)"
if [ -f "$TLOG" ] && [ -n "$(find "$TLOG" -mmin -5 2>/dev/null)" ]; then
  add_check V11.thermal PASS "thermal.log 近 5min 有记录（温度/负载历史可回溯）"
else
  add_check V11.thermal FAIL "thermal.log 缺失或超过 5min 未更新（khaen-thermal-log.timer 没跑？）"
fi

# V10 IRQ 亲和持久化（task-192；task-189 实测 irqbalance 会把 IRQ58/56/57 搬回 A72）
irq_config_load
if sunit_exists "$IRQ_BALANCE_UNIT"; then
  RB_EN="$(sunit_enabled_state "$IRQ_BALANCE_UNIT")"
  case "$RB_EN" in
    enabled|enabled-runtime) add_check V10.irqbalance FAIL "$IRQ_BALANCE_UNIT 仍 enabled（state=$RB_EN）：重启会重新均衡并把 IRQ 搬回 A72" ;;
    *)
      if sunit_active "$IRQ_BALANCE_UNIT"; then
        add_check V10.irqbalance FAIL "$IRQ_BALANCE_UNIT 仍 active：≤25s 内会把 IRQ58/56/57 搬回 A72"
      else
        add_check V10.irqbalance PASS "$IRQ_BALANCE_UNIT inactive 且未 enabled（state=${RB_EN:-unknown}）"
      fi ;;
  esac
else
  add_check V10.irqbalance PASS "$IRQ_BALANCE_UNIT 未安装（无自动均衡；oneshot 仍钉核）"
fi
if sunit_exists khaen-irq-affinity.service; then
  if sunit_active khaen-irq-affinity.service; then
    add_check V10.unit PASS "khaen-irq-affinity.service active（host 启动前钉核 oneshot 已执行）"
  else
    add_check V10.unit FAIL "khaen-irq-affinity.service 未 active（host 启动前钉核未执行/失败）"
  fi
else
  add_check V10.unit FAIL "khaen-irq-affinity.service 未安装（无法证明 pin 在 host 之前生效）"
fi
# B1（2026-11-20 板窗）：oneshot 必须有开机等待前置；否则重启时 IRQ56/57 节点晚于 oneshot 就绪，
# apply fail-closed → host 因 Requires= 不启动（v11 首启 FAIL 的根因）。
BOOTWAIT_UNIT="$(p "$KHAEN_SYSTEMD_DIR/khaen-irq-affinity.service")"
if [ -f "$BOOTWAIT_UNIT" ] && grep -q '^ExecStartPre=/usr/local/lib/khaen/irq-affinity-pin wait' "$BOOTWAIT_UNIT"; then
  add_check V10.bootwait PASS "khaen-irq-affinity.service 含 ExecStartPre=irq-affinity-pin wait（B1 开机等 IRQ 节点）"
else
  add_check V10.bootwait FAIL "khaen-irq-affinity.service 缺 ExecStartPre=irq-affinity-pin wait（重启持久性无保证：IRQ56/57 晚到会使 host 不启动；重打 bundle 并重跑 04）"
fi
LEGACY_DROPIN="$(p "$KHAEN_SYSTEMD_DIR/khaen-irq-affinity.service.d/10-wait-irq.conf")"
if [ -e "$LEGACY_DROPIN" ]; then
  add_check V10.legacy-dropin FAIL "遗留 B1 live-only drop-in 仍存在：$LEGACY_DROPIN（语义已并入 unit；重跑 04 会迁移到 evidence/migrated-dropins/）"
else
  add_check V10.legacy-dropin PASS "无遗留 B1 live-only drop-in（由 unit 内置 ExecStartPre 取代）"
fi
if out="$(py "$DEPLOY_ROOT/lib/irq_affinity.py" check --root "$ROOT" --pins "$IRQ_PINS" --t45-map "$IRQ_T45_MAP" --dsp-cpus "$IRQ_DSP_CPUS" --json "$EVIDENCE_DIR/irq-affinity-check.json" 2>&1)"; then
  add_check V10.pins PASS "IRQ58/56/57 effective 仍在 A53（$IRQ_PINS）+ T45 IRQ 放置前置 PASS（plan 的 IRQ 维度，非 plan() 本体）"
else
  add_check V10.pins FAIL "IRQ 亲和/T45 前置失败：$(printf '%s' "$out" | tr '\n' ' ' | cut -c1-400)"
fi

# V7 RT 属性（软/硬）
if [ -n "$HOST_PID" ]; then
  LIM="$(cat "$(p /proc)/$HOST_PID/limits" 2>/dev/null || true)"
  RT_OK=0; ML_OK=0
  printf '%s' "$LIM" | grep -qE 'Max realtime priority[[:space:]]+[1-9]' && RT_OK=1
  printf '%s' "$LIM" | grep -qE 'Max locked memory[[:space:]]+unlimited' && ML_OK=1
  if [ "$RT_OK" = "1" ] && [ "$ML_OK" = "1" ]; then
    add_check V7.rt PASS "rtprio>0 且 memlock=unlimited"
  elif [ "$REQUIRE_RT" = "1" ]; then
    add_check V7.rt FAIL "RT 属性不满足（rtprio_ok=$RT_OK memlock_ok=$ML_OK）"
  else
    add_check V7.rt NOT_WIRED "RT 属性未满足/未测（rtprio_ok=$RT_OK memlock_ok=$ML_OK；--require-rt 才是硬门）"
  fi
else
  add_check V7.rt NOT_WIRED "无 host pid，RT 属性未测"
fi

# V8 health
if command -v curl >/dev/null 2>&1; then
  CODE="$(curl -s -m 3 -o /dev/null -w '%{http_code}' "$HEALTH_URL" 2>/dev/null || true)"
  if [ "$CODE" = "200" ]; then add_check V8.health PASS "GET $HEALTH_URL -> 200"
  elif [ "$REQUIRE_HEALTH" = "1" ]; then add_check V8.health FAIL "GET $HEALTH_URL -> ${CODE:-failed}"
  else add_check V8.health NOT_WIRED "health 未接通（code=${CODE:-none}；daemon NOT_BUILT，--require-health 才是硬门）"; fi
else
  add_check V8.health NOT_VERIFIED "curl 不可用"
fi

{
  printf '{\n "schema": "khaen-rt-stack/verify-report/v1",\n'
  printf ' "checked_at": "%s",\n "host_pid": "%s",\n "fails": %s,\n "soft": %s,\n "checks": [' "$(now_iso)" "${HOST_PID:-}" "$FAILS" "$SOFT"
  printf '%s' "${checks%,}"
  printf '\n ]\n}\n'
} > "$EVIDENCE_DIR/verify-report.json"
log "== verify: FAIL=$FAILS SOFT=$SOFT report=$EVIDENCE_DIR/verify-report.json"
[ "$FAILS" -eq 0 ] && exit 0
exit 5
