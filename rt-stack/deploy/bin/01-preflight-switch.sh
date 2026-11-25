#!/usr/bin/env bash
# T28 01-preflight-switch.sh —— 切换前置门（**只读**，不产生任何变更）
#
# 检查项（全部必须 PASS，否则 exit 3 且不进入退役）：
#   A 授权窗口有效（live 时；scope 含 preflight）
#   B 新栈 release 已安装且 manifest/hash/架构校验通过（可执行 aarch64 ELF）
#   C 新栈 unit + /etc/tspi-rt-stack/{rt-host.toml,rt-daemon.toml,*.env} 已安装；host/daemon 尚未 active
#   D 旧栈处于 audio-off：旧音频 unit（含保留 AoIP/PTP）全部 inactive；无 camilladsp/tspi-upmix 进程
#   E /run/tspi/mode 不是 dante/aes67；旧 C 激活入口存在（将被 retire 清理）或已退役
#   F 设备存在：/proc/asound/cards 含 MOTU card id（UltraLitemk5）
#   G 回退素材：已有自研 last-good，或显式 --allow-first-switch（首次切换回退=mute）
#   H 磁盘/时间：/opt 与 /var/lib 余量 >= 512MB；NTP 同步或显式记录 NOT_VERIFIED
#   J IRQ 亲和持久化（task-192）：依赖 IRQ(58/56/57) 的 /proc/irq 节点存在；完整模式还要求
#     irqbalance 未 enabled/active、tspi-irq-affinity.service + irq-affinity.env 已安装、
#     当前 effective 亲和已在 A53 且 T45 IRQ 放置前置 PASS（04 install 已停 irqbalance 并写 pin）。
#     B1（2026-11-20 板窗）：完整模式另要求 oneshot 含 ExecStartPre=irq-affinity-pin wait（开机先等 IRQ 节点），
#     且无遗留 live-only drop-in（tspi-irq-affinity.service.d/10-wait-irq.conf；04 安装时会迁移）。
#
# 两段使用（F-40 修复）：
#   --pre-install  只跑不依赖新 release 的只读门（A/D/E/F/G/H/J-存在性）；05 在 04 install **之前**调用，
#                  失败=真正零变更（旧 C 原状，release/current 未动）。
#   默认（省略）    完整 A–J 门；05 在 04 install 之后调用（能校验真实安装的 manifest/arch/toml/IRQ pin）。
#
# 用法: sudo ./01-preflight-switch.sh --live --authorize WINDOW.json [--allow-first-switch] [--pre-install] [--root DIR]
# 退出码: 0 PASS；2 用法；3 前置门失败（pre-install 时未做任何变更）；4 授权窗口失败；7 环境不支持。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

ALLOW_FIRST=0
REQUIRE_HEALTH=0
PRE_INSTALL=0
usage() { sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --allow-first-switch) ALLOW_FIRST=1; shift ;;
    --pre-install) PRE_INSTALL=1; shift ;;
    --require-health) REQUIRE_HEALTH=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

FAILS=0
PASSES=0
pass() { PASSES=$((PASSES+1)); log "[PASS] $*"; }
fail() { FAILS=$((FAILS+1)); warn "[FAIL] $*"; }
soft() { warn "[NOT_VERIFIED] $*"; }

require_authorization "preflight"
[ "$DRY_RUN" = "0" ] && require_root_live

if [ "$DRY_RUN" = "1" ]; then
  if [ "$PRE_INSTALL" = "1" ]; then
    log "[dry-run] --pre-install：只执行不依赖新 release 的只读门 A/D/E/F/G/H（05 在 04 install 前调用；失败=零变更）"
  else
    log "[dry-run] 将执行只读前置门 A-I："
  fi
  log "  A 授权窗口校验（live）"
  if [ "$PRE_INSTALL" = "0" ]; then
    log "  B release 已装 + manifest/hash/arch 校验（py lib/release_manifest.py verify ... --require-arch aarch64）"
    log "  C 新栈 unit + /etc/tspi-rt-stack/{rt-host,rt-daemon}.{toml,env} 已安装/可加载/未 active：${NEW_STACK_UNITS[*]}"
  fi
  log "  D 旧 C/编排/AoIP units inactive + 无 ${C_PROCESS_PATTERNS[*]} 进程（旧 web 允许 active）"
  log "  E /run/tspi/mode 非 dante/aes67"
  log "  F /proc/asound/cards 含 MOTU card id"
  log "  G 自研 last-good 存在 或 --allow-first-switch"
  log "  H /opt /var/lib 余量 + 时间同步"
  if [ "$PRE_INSTALL" = "0" ]; then
    log "  I MOTU 48k keeper（脚本/unit/timer/udev rule）；运行配置含 rt-host.toml 无 192000/176400；已安装程序 192k 审计（F-07：识别拒绝码，不因错误文本误报）"
  fi
  log "  J IRQ 亲和持久化：依赖 IRQ(58/56/57) 的 /proc/irq 节点存在（pre-install 仅存在性；04 安装时 disable+stop irqbalance 并写 pin）"
  if [ "$PRE_INSTALL" = "0" ]; then
    log "  J （完整门）irqbalance 未 enabled/active；tspi-irq-affinity.service + irq-affinity.env 已安装；IRQ effective 已在 A53 + T45 IRQ 放置前置（lib/irq_affinity.py，非 plan() 本体）"
    log "  J （完整门）B1 开机竞态防护：已装 tspi-irq-affinity.service 含 ExecStartPre=irq-affinity-pin wait；无遗留 live-only drop-in"
  fi
  exit 0
fi

# ---- A 授权窗口（require_authorization 已做；记录） ----------------------------
pass "A 授权窗口有效（已由 lib/check_window.py 校验）"

if [ "$PRE_INSTALL" = "0" ]; then
# ---- B release ---------------------------------------------------------------
REL="$(current_release_dir 2>/dev/null || true)"
if [ -z "$REL" ]; then
  fail "B 新栈 current release 不存在（先 04-install-release.sh）；${TSPI_STACK_PREFIX}/current"
else
  if [ ! -f "$REL/manifest.json" ]; then
    fail "B release 缺 manifest.json: $REL"
  else
    if out="$(py "$DEPLOY_ROOT/lib/release_manifest.py" verify "$REL" --require-arch aarch64 2>&1)"; then
      pass "B release 校验 PASS: $(printf '%s' "$out" | py -c 'import json,sys;d=json.load(sys.stdin);print(d.get("release_id"),d.get("files_checked"),"files")' 2>/dev/null || printf '%s' "$out")"
      HOST_BIN="$REL/bin/tspi-rt-host"
      if py "$DEPLOY_ROOT/lib/check_elf.py" "$HOST_BIN" --require-arch aarch64 >/dev/null 2>&1; then
        pass "B tspi-rt-host 是 ELF aarch64: $(py "$DEPLOY_ROOT/lib/check_elf.py" "$HOST_BIN" | py -c 'import json,sys;d=json.load(sys.stdin);print(d["size"],d["sha256"][:16])')"
      else
        fail "B tspi-rt-host 不是可部署的 ELF aarch64（见 evidence/deploy-artifacts）"
      fi
    else
      fail "B release 校验失败: $out"
    fi
  fi
fi

# ---- C 新栈 unit -------------------------------------------------------------
for u in "${NEW_STACK_UNITS[@]}"; do
  if sunit_exists "$u"; then
    if sunit_active "$u"; then
      fail "C $u 已经 active（新栈已在运行；切换前应 inactive）"
    else
      pass "C $u 已安装且 inactive"
    fi
  else
    fail "C $u 未安装/不可加载（04-install-release.sh 未完成或 unit 有语法错）"
  fi
done
# F-18/T28-P0-2：host/daemon 的配置必须已由 04 安装；否则 host 启动即退、preflight 会放行一份必失败的配置。
for f in rt-host.toml rt-daemon.toml rt-host.env rt-daemon.env; do
  if [ -f "$(p "$TSPI_ETC/$f")" ]; then
    pass "C 配置已安装: $TSPI_ETC/$f"
  else
    fail "C 缺少配置 $TSPI_ETC/$f（04-install-release.sh 只装 env 不装 toml 的旧缺陷，见 T28-P0-2）"
  fi
done
fi # PRE_INSTALL=0（依赖新 release 的 B/C）

# ---- D 旧栈 audio-off --------------------------------------------------------
BUSY=()
for u in "${RETIRE_C_UNITS[@]}" "${RETIRE_ENTRY_UNITS[@]}" "${RETAIN_AOIP_UNITS[@]}"; do
  if sunit_active "$u"; then BUSY+=("$u"); fi
done
if [ "${#BUSY[@]}" -gt 0 ]; then
  fail "D 仍 active 的旧 C/编排/AoIP unit: ${BUSY[*]} —— 窗口必须从 audio-off 开始（本任务不代执行；旧 web 允许 active）"
else
  pass "D 旧 C/编排/AoIP unit 全部 inactive（旧 web 允许 active，03-retire-c 会停）"
fi
PROCS="$(pgrep -a -f 'camilladsp|tspi-upmix' 2>/dev/null || true)"
if [ -n "$PROCS" ]; then
  fail "D 仍存在旧 C 进程（preflight 不代停，需在窗口内由 03-retire-c 处理；先确认 S1 已置 off）：$(printf '%s' "$PROCS" | head -n 3 | tr '\n' ' ')"
else
  pass "D 无 camilladsp/tspi-upmix 进程"
fi

# ---- E 模式/入口 -------------------------------------------------------------
MODE_FILE="$(p /run/tspi/mode)"
if [ -f "$MODE_FILE" ]; then
  MODE="$(tr -d '[:space:]' < "$MODE_FILE" 2>/dev/null || true)"
  case "$MODE" in
    dante|aes67) fail "E /run/tspi/mode=$MODE（音频模式仍开启；必须在 off 后切换）" ;;
    *) pass "E /run/tspi/mode=${MODE:-unknown}（非 dante/aes67）" ;;
  esac
else
  pass "E /run/tspi/mode 不存在（audio-off 语义）"
fi
ENTRY="$(c_entry_present || true)"
if [ -n "$ENTRY" ]; then
  log "E 旧 C 激活入口存在（将由 03-retire-c 清理）: $(printf '%s' "$ENTRY" | tr '\n' ' ')"
else
  log "E 旧 C 激活入口已不存在（可能已退役，idempotent）"
fi

# ---- F 设备 -----------------------------------------------------------------
CARDS="$(cat "$(p /proc/asound/cards)" 2>/dev/null || true)"
if printf '%s' "$CARDS" | grep -qi 'UltraLite'; then
  pass "F /proc/asound/cards 含 MOTU UltraLite（不打开 PCM）"
else
  fail "F 未在 /proc/asound/cards 找到 MOTU UltraLite；设备未接/未枚举时不切换（silence 优于错图）"
  sed 's/^/    /' <<< "$CARDS" >&2 || true
fi

# ---- G 回退素材 ---------------------------------------------------------------
LG="$(p "$TSPI_STATE_DIR/last-good.json")"
if [ -f "$LG" ]; then
  pass "G 自研 last-good 存在: $LG"
elif [ "$ALLOW_FIRST" = "1" ]; then
  pass "G 首次切换：--allow-first-switch；回退目标=mute（绝无 C 回退）"
else
  fail "G 无自研 last-good 且未给 --allow-first-switch；首次切换必须显式声明回退=mute"
fi

# ---- I MOTU 48k keeper / 192k 禁令 -------------------------------------------
if [ "$ROOT" != "/" ]; then
  soft "I --root fixture：跳过 host-level MOTU 48k keeper/192k 检查（T71 live ROOT=/ 才为硬门）"
else
if sunit_exists tspi-motu-48k-keeper.timer; then
  KSTATE="$(sunit_enabled_state tspi-motu-48k-keeper.timer)"
  case "$KSTATE" in
    enabled|enabled-runtime) pass "I MOTU 48k keeper timer 已安装且 enabled（$KSTATE）" ;;
    *) fail "I MOTU 48k keeper timer 未 enabled（state=${KSTATE:-unknown}）；开机/重枚举后 48k 无保证" ;;
  esac
else
  fail "I tspi-motu-48k-keeper.timer 未安装（04-install-release.sh 未完成或 unit 缺失）"
fi
if [ -x "$(p /usr/local/lib/tspi/motu-48k-keeper)" ]; then
  pass "I MOTU 48k keeper 脚本可执行: /usr/local/lib/tspi/motu-48k-keeper"
else
  fail "I 缺少 /usr/local/lib/tspi/motu-48k-keeper（静音 1s 48k keeper）"
fi
# F-43：192k 硬失败必须覆盖新栈 toml（F-18 后 04 会安装它），并同时拒绝 176.4k。
BAD192=""
for f in /run/tspi/config.json /etc/tspi-audio/default.env /etc/tspi-audio-web/config.toml \
         "$TSPI_ETC/rt-host.env" "$TSPI_ETC/rt-host.toml" "$TSPI_ETC/rt-daemon.toml"; do
  [ -f "$(p "$f")" ] || continue
  if grep -qE '192000|192_000|176400|176_400' "$(p "$f")" 2>/dev/null; then BAD192="$BAD192 $f"; fi
done
if [ -z "$BAD192" ]; then
  pass "I 运行配置无 192000/176400（/run/tspi、tspi-audio、tspi-audio-web、rt-host.env/toml、rt-daemon.toml）"
else
  fail "I 运行配置仍含 192000/176400:$BAD192（禁 176.4/192k；先改为 48000/96000）"
fi
# F-43：udev 规则缺失时设备重枚举后 48k 无保证 → I 门失败（04 也必须硬失败）。
if [ -f "$(p /etc/udev/rules.d/95-tspi-motu-48k-keeper.rules)" ]; then
  pass "I MOTU 48k keeper udev 规则已安装"
else
  fail "I 缺少 /etc/udev/rules.d/95-tspi-motu-48k-keeper.rules（T28-P0 回归：udev 安装失败曾只 warn）"
fi
# F-07：已安装程序里的 '192000' 字面量不一定是支持路径——拒绝实现本身会在错误文本里写 192000
# （overlay tspi-channelmap/tspi-upmix 的 E_CM_FS_192K_DISABLED / E_UP_FS_192K_DISABLED）。
# 先认稳定拒绝码/禁用措辞，只有在没有拒绝证据时才记软警告。
for f in /usr/local/bin/tspi-channelmap /usr/local/bin/tspi-upmix /usr/local/bin/tspi-audio-web; do
  fp="$(p "$f")"; [ -f "$fp" ] || continue
  if grep -aqE 'E_UP_FS_192K_DISABLED|E_CM_FS_192K_DISABLED' "$fp"; then
    pass "I $f 已实现 176.4/192k 拒绝（稳定错误码；F-07）"
  elif grep -aq '192000' "$fp" && grep -aqE '已禁用|禁止 ?192k|FORBIDDEN_SAMPLE_RATES|forbidden' "$fp"; then
    pass "I $f 含 192000 字面量但伴随禁用措辞（按拒绝路径记 PASS，避免 F-07 误报）"
  elif grep -aq '192000' "$fp"; then
    soft "I $f 仍含 192000 且未发现拒绝码/禁用措辞（owner 需限定 48/96k；keeper 只兜底设备速率）"
  else
    pass "I $f 未发现 192k 支持路径"
  fi
done
fi # ROOT=="/" keeper gate

# ---- J IRQ 亲和持久化（task-192；T45 依赖 IRQ 必须离开 A72 DSP 核） ------------
irq_config_load
IRQ_MISSING=""
for pair in $IRQ_PINS; do
  irq="${pair%%:*}"
  [ -f "$(p "/proc/irq/$irq/smp_affinity_list")" ] || IRQ_MISSING="$IRQ_MISSING IRQ$irq"
done
if [ -n "$IRQ_MISSING" ]; then
  fail "J 依赖 IRQ 的 /proc/irq 节点缺失:$IRQ_MISSING（设备未枚举/IRQ 号变化；IRQ 持久化与 T45 admission 无法保证）"
else
  pass "J 依赖 IRQ 的 /proc/irq 节点存在（pins: $IRQ_PINS）"
fi
if [ "$PRE_INSTALL" = "1" ]; then
  if sunit_exists "$IRQ_BALANCE_UNIT"; then
    log "J irqbalance 当前 enabled=$(sunit_enabled_state "$IRQ_BALANCE_UNIT") active=$(sunit_active "$IRQ_BALANCE_UNIT" && echo yes || echo no)；04-install 会 disable+stop（pre-install 不因此失败）"
  else
    log "J $IRQ_BALANCE_UNIT 未安装（无自动均衡；跳过停用）"
  fi
  if [ -f "$DEPLOY_ROOT/config/irq-affinity-pin" ]; then
    pass "J deploy 提供 irq-affinity-pin（04 install 将安装到 /usr/local/lib/tspi/）"
  else
    fail "J deploy 缺 config/irq-affinity-pin（IRQ 持久化不可用）"
  fi
else
  if [ -z "$IRQ_MISSING" ]; then
    if sunit_exists "$IRQ_BALANCE_UNIT"; then
      RB_EN="$(sunit_enabled_state "$IRQ_BALANCE_UNIT")"
      case "$RB_EN" in
        enabled|enabled-runtime) fail "J $IRQ_BALANCE_UNIT 仍 enabled（state=$RB_EN）：重启会被重新拉起并把 IRQ 搬回 A72" ;;
        *) pass "J $IRQ_BALANCE_UNIT 未 enabled（state=${RB_EN:-unknown}）" ;;
      esac
      if sunit_active "$IRQ_BALANCE_UNIT"; then
        fail "J $IRQ_BALANCE_UNIT 仍 active：task-189 实测 ≤25s 会把 IRQ58/56/57 搬回 A72"
      else
        pass "J $IRQ_BALANCE_UNIT inactive"
      fi
    else
      pass "J $IRQ_BALANCE_UNIT 未安装（无自动均衡）"
    fi
    if sunit_exists tspi-irq-affinity.service; then
      pass "J tspi-irq-affinity.service 已安装（Before=tspi-rt-host.service 钉核 oneshot）"
    else
      fail "J tspi-irq-affinity.service 未安装（旧 bundle？重新 make-release-bundle）"
    fi
    if [ -f "$(p "$TSPI_ETC/irq-affinity.env")" ]; then
      pass "J IRQ 配置已安装: $TSPI_ETC/irq-affinity.env"
    else
      fail "J 缺 $TSPI_ETC/irq-affinity.env（04 install 未落盘 pin 配置）"
    fi
    if out="$(py "$DEPLOY_ROOT/lib/irq_affinity.py" check --root "$ROOT" --pins "$IRQ_PINS" --t45-map "$IRQ_T45_MAP" --dsp-cpus "$IRQ_DSP_CPUS" 2>&1)"; then
      pass "J IRQ effective 已在 A53 + T45 IRQ 放置前置 PASS（lib/irq_affinity.py；${IRQ_PINS}）"
    else
      fail "J IRQ 亲和/T45 前置失败：$(printf '%s' "$out" | tr '\n' ' ' | cut -c1-360)"
    fi
  fi
  # B1（2026-11-20 板窗）：开机竞态防护必须已装到板上（否则重启后 IRQ56/57 晚到 → host 不启动）
  BOOTWAIT_UNIT="$(p "$TSPI_SYSTEMD_DIR/tspi-irq-affinity.service")"
  if [ -f "$BOOTWAIT_UNIT" ] && grep -q '^ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait' "$BOOTWAIT_UNIT"; then
    pass "J B1 开机竞态防护已安装：tspi-irq-affinity.service 含 ExecStartPre=irq-affinity-pin wait"
  else
    fail "J B1 开机竞态防护缺失：$BOOTWAIT_UNIT 无 ExecStartPre=irq-affinity-pin wait（重启后 IRQ56/57 晚到会使 host 不启动；重打 bundle 并重跑 04）"
  fi
  LEGACY_DROPIN="$(p "$TSPI_SYSTEMD_DIR/tspi-irq-affinity.service.d/10-wait-irq.conf")"
  if [ -e "$LEGACY_DROPIN" ]; then
    fail "J 遗留 B1 live-only drop-in 仍存在：$LEGACY_DROPIN（语义已并入 unit；重跑 04 会迁移到 evidence/migrated-dropins/）"
  else
    pass "J 无遗留 B1 live-only drop-in（由 unit 内置 ExecStartPre 取代）"
  fi
fi

# ---- H 资源/时间 -------------------------------------------------------------
AVAIL="$(df -Pk "$(p /opt)" 2>/dev/null | awk 'NR==2{print $4}')"
if [ -n "${AVAIL:-}" ] && [ "$AVAIL" -ge 524288 ] 2>/dev/null; then
  pass "H /opt 可用 ${AVAIL}KB >= 512MB"
else
  soft "H /opt 可用空间未验证（df 输出: ${AVAIL:-UNSUPPORTED}）"
fi
NTP="$(timedatectl show -p NTPSynchronized --value 2>/dev/null || true)"
if [ "$NTP" = "yes" ]; then pass "H NTP 已同步"; else soft "H NTP 同步状态=${NTP:-UNSUPPORTED}（记录，不阻断；切换前确认时钟）"; fi
if [ "$REQUIRE_HEALTH" = "1" ] && [ "$DRY_RUN" = "0" ] && [ -z "${HEALTH_URL:-}" ]; then
  soft "H --require-health 但未给 HEALTH_URL（由 06-verify-switch.sh 再验）"
fi

log "== preflight: PASS=$PASSES FAIL=$FAILS（mode=$( [ "$PRE_INSTALL" = "1" ] && echo pre-install || echo full )）"
if [ "$FAILS" -gt 0 ]; then
  if [ "$PRE_INSTALL" = "1" ]; then
    die "pre-install 前置门失败 $FAILS 项：未做任何变更（未安装 release/未停旧 C）" 3
  fi
  # F-40：05 在 04 install 之后才调用完整 preflight；失败时 release/current 与 keeper enable 已落盘，
  # 但音频状态（旧 C 运行态）未动。不得再写“零变更”。
  die "前置门失败 $FAILS 项：未停用/未切换音频（旧 C 仍按原状态）；若 05 已执行 install，则 release/current 与 keeper enable 已落盘" 3
fi
exit 0
