#!/usr/bin/env bash
# T28 08-rollback.sh —— 切换失败/运行失败时的安全恢复（**只允许自研 last-good 或 mute；绝不回旧 C**）
#
# 模式：
#   --to-last-good（默认）  切回 /var/lib/tspi-rt-stack/last-good.json 记录的 release；校验失败/启动失败→mute
#   --to-previous           用 last-good.previous 记录的更早 release（仍为自研），失败→mute
#   --mute                  不切 release：安装 safe-mute drop-in 并启动 host（保护静音）
#
# P1 硬约束：last-good 的 c_policy 必须是 never-camilladsp；C 永不作为回退目标。
# 硬约束：
#   * 启动前断言旧 C 已退役（unit 未 enabled/active、无 camilladsp/tspi-upmix 进程）；若发现 C 可能复活→exit 6，不动它；
#   * 本脚本从不 stop/start/enable/disable 任何旧 C unit，也不改旧 C 配置；
#   * 最终状态：0=回到 last-good；10=降级 mute（服务在、静音）；6=静音/停止（C 保持 masked，需人工）。
#
# 用法: sudo ./08-rollback.sh --live --authorize WINDOW.json [--to-last-good|--to-previous|--mute] [--evidence DIR]
# 退出码: 0/6/10 见上；2 用法；4 授权；5 状态冲突（例如新栈停不下来）。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

MODE="last-good"
MUTE_FALLBACK=1
TIMEOUT="${TSPI_TIMEOUT_SECS:-120}"
usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --to-last-good) MODE="last-good"; shift ;;
    --to-previous) MODE="previous"; shift ;;
    --mute) MODE="mute"; shift ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --evidence) EVIDENCE_DIR="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --no-mute-fallback) MUTE_FALLBACK=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

require_authorization "rollback"
[ "$DRY_RUN" = "0" ] && require_root_live
export TSPI_DEPLOY_ROOT="$ROOT"

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 模式=$MODE：将停止新栈 → 断言 C 已退役 → 切换 release 或安装 safe-mute drop-in → 启动/验证"
  log "[dry-run] 禁止动作：绝不 systemctl start/enable camilladsp 或 tspi-upmix-autodetect；绝不改旧 C 配置"
  exit 0
fi

evidence_prepare "08-rollback"
STACK_DIR="$(p "$TSPI_STACK_PREFIX")"
CUR="$STACK_DIR/current"
LG="$(p "$TSPI_STATE_DIR/last-good.json")"
DROPIN_DIR="$(p "$TSPI_SYSTEMD_DIR")/tspi-rt-host.service.d"
DROPIN="$DROPIN_DIR/90-safe-mute.conf"
MUTE_ENV="$(p "$TSPI_ETC")/90-safe-mute.env"
# F-17：safe-mute 用 EnvironmentFile 覆盖 rt-host.env 的 normal（systemd.exec：EnvironmentFile 覆盖 Environment=），
# 因此要同时安装 90-safe-mute.conf 与 90-safe-mute.env；两者都从 service/ 或已安装 release units 解析。
# F5（2026-11-18）：mute 生效核对不再用 `systemctl show -p Environment`（不含 EnvironmentFile 内容，
# 板端实测假阴性）；改为 lib/common.sh::unit_env_contains 的 /proc/<MainPID>/environ → EnvironmentFiles 解析。
resolve_dropin_src() {
  local name="$1" cand
  for cand in "$DEPLOY_ROOT/../service/tspi-rt-host.service.d/$name" \
              "$(current_release_dir 2>/dev/null || true)/units/$name" \
              "$(p "$TSPI_STACK_PREFIX")/current/units/$name"; do
    [ -n "$cand" ] && [ -f "$cand" ] && { printf "%s" "$cand"; return 0; }
  done
  return 1
}
DROPIN_SRC="$(resolve_dropin_src 90-safe-mute.conf.example || true)"
MUTE_ENV_SRC="$(resolve_dropin_src 90-safe-mute.env.example || true)"
ATTEMPTS=()
FINAL=""
EXIT_CODE=6

log "== 08 rollback 模式=$MODE（DRY_RUN=0）"

# ---- 0 断言旧 C 已退役；发现 C enabled/active = 禁止状态，不动它 ------------------
BAD="$(assert_c_retired || true)"
if [ -n "$(printf '%s' "$BAD" | tr -d '[:space:]')" ]; then
  warn "检测到旧 C 可能被激活（$(printf '%s' "$BAD" | tr '\n' ' ')）：rollback 拒绝任何 start；保持现状并升级人工处理"
  printf '{\n "schema":"tspi-rt-stack/rollback-report/v1",\n "final_state":"FORBIDDEN_C_STATE",\n "exit":6,\n "at":"%s",\n "detail":"%s"\n}\n' "$(now_iso)" "$(printf '%s' "$BAD" | tr '\n' ' ')" > "$EVIDENCE_DIR/rollback-report.json"
  exit 6
fi
ATTEMPTS+=("assert_c_retired: PASS（C 未 enabled/active、无 C 进程）")

snapshot_state() { # 记录当前新栈状态
  {
    echo "current=$(readlink -f "$CUR" 2>/dev/null || echo none)"
    for u in "${NEW_STACK_UNITS[@]}"; do echo "$u active=$("$TSPI_SYSTEMCTL" is-active "$u" 2>/dev/null || true) enabled=$(sunit_enabled_state "$u")"; done
    pgrep -a -f 'tspi-rt-(host|daemon)' 2>/dev/null || true
  } > "$EVIDENCE_DIR/pre-rollback-state.txt" 2>&1 || true
}
snapshot_state

# ---- 1 停止新栈（不能停就退出，避免双实例） -------------------------------------
stop_new_stack() {
  local where="$1" u
  for u in tspi-rt-daemon.service tspi-rt-host.service; do
    if sunit_active "$u"; then
      if ! "$TSPI_SYSTEMCTL" stop "$u"; then ATTEMPTS+=("stop $u: FAIL"); return 1; fi
      ATTEMPTS+=("stop $u: OK")
    fi
  done
  if sunit_active tspi-rt-stack.target; then "$TSPI_SYSTEMCTL" stop tspi-rt-stack.target >/dev/null 2>&1 || true; fi
  # 等待 inactive（daemon 不强制）
  local i=0
  while [ "$i" -lt "$TIMEOUT" ]; do
    sunit_active tspi-rt-host.service || return 0
    sleep "${TSPI_POLL_SECS:-1}"; i=$((i+1))
  done
  ATTEMPTS+=("wait inactive: TIMEOUT ($where)")
  return 1
}
if ! stop_new_stack "rollback"; then
  warn "新栈无法停止：不启动第二个实例；保持现状（静音风险），升级人工处理"
  FINAL="STOP_FAILED_RUNNING"; 
  printf '{\n "schema":"tspi-rt-stack/rollback-report/v1",\n "final_state":"%s",\n "exit":5,\n "at":"%s",\n "attempts":["%s"]\n}\n' "$FINAL" "$(now_iso)" "$(printf '%s' "${ATTEMPTS[*]}" | sed 's/"/\\"/g')" > "$EVIDENCE_DIR/rollback-report.json"
  exit 5
fi

install_mute_dropin() {
  if [ ! -f "$DROPIN_SRC" ]; then ATTEMPTS+=("mute drop-in 模板缺失: $DROPIN_SRC"); return 1; fi
  mkdir -p "$DROPIN_DIR" || return 1
  install -m 0644 "$DROPIN_SRC" "$DROPIN" || return 1
  ATTEMPTS+=("install $DROPIN")
  # env 文件：优先模板；旧 bundle 没有模板时写入最小等效内容（保证 mute 变量实际存在）。
  mkdir -p "$(dirname "$MUTE_ENV")" || return 1
  if [ -n "$MUTE_ENV_SRC" ] && [ -f "$MUTE_ENV_SRC" ]; then
    install -m 0640 "$MUTE_ENV_SRC" "$MUTE_ENV" || return 1
  else
    printf '# 08-rollback.sh --mute 生成（旧 bundle 无模板时的等价内容；F-17）\nTSPI_HOST_SAFE_MODE=mute\n' > "$MUTE_ENV" || return 1
  fi
  ATTEMPTS+=("install $MUTE_ENV")
  return 0
}
remove_mute_dropin() {
  if [ -f "$DROPIN" ]; then rm -f "$DROPIN"; ATTEMPTS+=("remove $DROPIN"); fi
  if [ -f "$MUTE_ENV" ]; then rm -f "$MUTE_ENV"; ATTEMPTS+=("remove $MUTE_ENV"); fi
  return 0
}
start_and_wait() { # unit
  local u="$1"
  "$TSPI_SYSTEMCTL" daemon-reload || return 1
  "$TSPI_SYSTEMCTL" start "$u" || return 1
  wait_unit_active "$u" "$TIMEOUT"
}
switch_current() { # release dir
  local rel="$1"
  [ -d "$rel" ] || return 1
  local mode
  mode="$(set_current "$rel")" || return 1
  ATTEMPTS+=("current -> $rel (mode=$mode)")
}

# ---- 2 解析回退目标 ------------------------------------------------------------
TARGET_REL=""
if [ "$MODE" = "mute" ]; then
  ATTEMPTS+=("mode=mute：不切 release")
else
  if [ ! -f "$LG" ]; then
    if [ "$MUTE_FALLBACK" = "1" ]; then ATTEMPTS+=("无 last-good.json -> 降级 mute"); MODE="mute"
    else ATTEMPTS+=("无 last-good.json 且 --no-mute-fallback"); FINAL="NO_LAST_GOOD"; fi
  else
    if out="$(py "$DEPLOY_ROOT/lib/last_good.py" validate "$LG" --require-release 2>&1)"; then
      TARGET_REL="$(printf '%s' "$out" | py -c 'import json,sys;print(json.load(sys.stdin).get("release_dir") or "")')"
      if [ "$MODE" = "previous" ]; then
        TARGET_REL="$(printf '%s' "$out" | py -c 'import json,sys;p=json.load(sys.stdin).get("previous") or {};print(p.get("release_dir") or "")')"
        [ -n "$TARGET_REL" ] || { ATTEMPTS+=("previous 为空 -> 降级 mute"); MODE="mute"; }
      fi
      ATTEMPTS+=("last-good validate: PASS target=${TARGET_REL:-none}")
    else
      ATTEMPTS+=("last-good validate: FAIL（$out）")
      if [ "$MUTE_FALLBACK" = "1" ]; then ATTEMPTS+=("降级 mute"); MODE="mute"; else FINAL="LAST_GOOD_INVALID"; fi
    fi
  fi
fi

# ---- 3 release 回退尝试 ---------------------------------------------------------
if [ "$MODE" != "mute" ] && [ -z "$FINAL" ] && [ -n "$TARGET_REL" ]; then
  if switch_current "$TARGET_REL" && remove_mute_dropin && start_and_wait tspi-rt-host.service; then
    V_ARGS=(--live --timeout "$TIMEOUT" --allow-retained-active --evidence "$EVIDENCE_DIR/verify")
    if "$HERE/06-verify-switch.sh" "${V_ARGS[@]}"; then
      FINAL="ROLLED_BACK_LAST_GOOD"; EXIT_CODE=0
      ATTEMPTS+=("verify: PASS")
    else
      ATTEMPTS+=("verify: FAIL -> 降级 mute")
      "$TSPI_SYSTEMCTL" stop tspi-rt-host.service >/dev/null 2>&1 || true
      MODE="mute"
    fi
  else
    ATTEMPTS+=("release 回退启动失败 -> 降级 mute")
    "$TSPI_SYSTEMCTL" stop tspi-rt-host.service >/dev/null 2>&1 || true
    MODE="mute"
  fi
fi

# ---- 4 mute 兜底 ---------------------------------------------------------------
if [ "$MODE" = "mute" ] && [ -z "$FINAL" ]; then
  if install_mute_dropin && start_and_wait tspi-rt-host.service; then
    if unit_env_contains tspi-rt-host.service "TSPI_HOST_SAFE_MODE=mute"; then
      ATTEMPTS+=("mute env: 已生效（核对 $(unit_env_verify_source tspi-rt-host.service)；EnvironmentFile 覆盖 rt-host.env 的 normal）")
    else
      ATTEMPTS+=("mute env: NOT_APPLIED（MainPID /proc environ 与 EnvironmentFiles 均未含 TSPI_HOST_SAFE_MODE=mute；服务在=静音，需人工核对 drop-in/env）")
    fi
    FINAL="DEGRADED_MUTE"; EXIT_CODE=10
  else
    ATTEMPTS+=("mute 启动失败 -> 停止新栈（静音）")
    "$TSPI_SYSTEMCTL" stop tspi-rt-host.service >/dev/null 2>&1 || true
    "$TSPI_SYSTEMCTL" stop tspi-rt-stack.target >/dev/null 2>&1 || true
    FINAL="STOPPED_SILENT_C_MASKED"; EXIT_CODE=6
  fi
fi
[ -n "$FINAL" ] || { FINAL="STOPPED_SILENT_C_MASKED"; EXIT_CODE=6; }

# ---- 5 末尾再断言 C 未复活 ------------------------------------------------------
BAD2="$(assert_c_retired || true)"
if [ -n "$(printf '%s' "$BAD2" | tr -d '[:space:]')" ]; then
  warn "回退过程中检测到 C 状态异常：$(printf '%s' "$BAD2" | tr '\n' ' ')"
  FINAL="FORBIDDEN_C_STATE"; EXIT_CODE=6
fi

python_body=""
{
  printf '{\n "schema": "tspi-rt-stack/rollback-report/v1",\n'
  printf ' "mode": "%s",\n "final_state": "%s",\n "exit": %s,\n "at": "%s",\n' "$MODE" "$FINAL" "$EXIT_CODE" "$(now_iso)"
  printf ' "attempts": [\n'
  i=0
  for a in "${ATTEMPTS[@]}"; do
    i=$((i+1)); [ "$i" -eq 1 ] || printf ',\n'
    printf '  "%s"' "$(printf '%s' "$a" | sed 's/\\/\\\\/g; s/"/\\"/g')"
  done
  printf '\n ]\n}\n'
} > "$EVIDENCE_DIR/rollback-report.json"
log "== 08 rollback: $FINAL (exit=$EXIT_CODE) report=$EVIDENCE_DIR/rollback-report.json"
exit "$EXIT_CODE"
