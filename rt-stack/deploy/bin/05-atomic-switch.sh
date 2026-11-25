#!/usr/bin/env bash
# T28 05-atomic-switch.sh —— 授权窗口内**原子切换**总编排（T71 唯一入口）
#
# 实做顺序（F-45：文件头必须与实做一致；每步仍是独立脚本，可单独审计）——
#   00 snapshot（只读）
#   → 01 preflight --pre-install（只读；不依赖新 release，失败=完全零变更）
#   → 04 install-release（落盘 release/current + 装 toml/env + keeper enable；不 start）
#   → 01 preflight（完整 A–I；校验真实安装的 manifest/arch/toml）
#   → 03 retire-c（停/禁/mask 旧 C；从这里起不可回 C）
#   → 02 device 独占（只读）
#   → start MOTU 48k keeper → start khaen-irq-affinity.service（task-192：host 启动前钉 T45 依赖
#     IRQ58/56/57→A53；失败=host 不启动）→ enable/start khaen-rt-stack.target（Wants host+daemon；逐个等 active）
#   → 06 verify → 07 mark-last-good
# 失败策略（硬约束）：
#   * install 之前的任何失败：exit 3，**完全零变更**（旧 C 原状，release/current 未动）
#   * install 之后、retire 之前的失败：exit 3，**音频零变更**（release/current、keeper enable 已落盘）
#   * retire 之后任何失败：只允许 08-rollback.sh（自研 last-good 或 mute），**绝不启动/使能旧 C**
#     - rollback 成功回到 last-good：exit 5（切换失败、已恢复）
#     - 降级为 mute：exit 10（服务在但静音）
#     - rollback 失败进入静音/停止：exit 6（C 保持 masked）
#   * 已通过验证但 last-good 写失败：exit 8（RUNNING_NO_LAST_GOOD，不中断播放，人工补记；F-41 独立码）
#
# 用法:
#   sudo ./05-atomic-switch.sh --live --authorize WINDOW.json --bundle /var/tmp/khaen-rt-stack-release-<id> \
#        [--allow-first-switch] [--require-daemon] [--require-health --health-url URL] [--timeout 120]
#   ./05-atomic-switch.sh --dry-run ...      # 默认；只打印计划，不调用任何变更命令
# 退出码：0 成功；2 用法；3 前置/安装失败（音频零变更）；4 授权；5 切换失败已回滚；6 失败且静音/停止；
#         8 已验证运行但 last-good 未记录；10 降级 mute。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

BUNDLE=""
TIMEOUT="${KHAEN_TIMEOUT_SECS:-120}"
ALLOW_FIRST=0
REQUIRE_DAEMON=0
REQUIRE_RT=0
REQUIRE_HEALTH=0
HEALTH_URL="${HEALTH_URL:-}"
usage() { sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --bundle) BUNDLE="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --evidence) EVIDENCE_DIR="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --allow-first-switch) ALLOW_FIRST=1; shift ;;
    --require-daemon) REQUIRE_DAEMON=1; shift ;;
    --require-rt) REQUIRE_RT=1; shift ;;
    --require-health) REQUIRE_HEALTH=1; shift ;;
    --health-url) HEALTH_URL="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

# 子脚本通过环境继承 staging root（本地夹具/预演不得落到真实 /）
export KHAEN_DEPLOY_ROOT="$ROOT"
AUTH_ARGS=()
[ -n "$AUTHORIZE_FILE" ] && AUTH_ARGS=(--authorize "$AUTHORIZE_FILE")
FIRST_ARGS=()
[ "$ALLOW_FIRST" = "1" ] && FIRST_ARGS=(--allow-first-switch)

if [ "$DRY_RUN" = "1" ]; then
  log "== 05 atomic-switch 计划（DRY-RUN；不会发生任何变更）=="
  log " 1) 00-snapshot-old-c.sh --live --out <evidence>/snapshot（只读）"
  log " 2) 01-preflight-switch.sh --live --pre-install ${AUTH_ARGS[*]} ${FIRST_ARGS[*]}（只读；失败=完全零变更）"
  log " 3) 04-install-release.sh --live --no-enable --authorize ... --bundle '$BUNDLE'（release/current + toml/env + keeper enable；不 start）"
  log " 4) 01-preflight-switch.sh --live ${AUTH_ARGS[*]} ${FIRST_ARGS[*]}（完整 A–I：manifest/arch/unit/toml/192k/keeper）"
  log " 5) 03-retire-c.sh --live ${AUTH_ARGS[*]} --evidence <evidence>/retire（从这里起不可回 C）"
  log " 6) 02-check-device-exclusive.sh --live（retire 后确认设备独占）"
  log " 7) systemctl start khaen-motu-48k-keeper.service（静音 1s 48k，禁止 192k）→ systemctl start khaen-irq-affinity.service（task-192：钉 IRQ58/56/57→A53 并回读）→ enable/start khaen-rt-stack.target（Wants host+daemon；逐个等 active <= ${TIMEOUT}s）"
  log " 8) 06-verify-switch.sh --live ... --evidence <evidence>/verify"
  log " 9) 07-mark-last-good.sh --live ${AUTH_ARGS[*]} --verify-report <evidence>/verify/verify-report.json"
  log " 失败：install 前=完全零变更；install 后/retire 前=音频零变更（release 已落盘）；retire 后只 08-rollback.sh（last-good 或 mute，绝不 C）。"
  exit 0
fi

require_authorization "start-new"
require_root_live
evidence_prepare "05-atomic-switch"
STAGES="$EVIDENCE_DIR/stages.log"
: > "$STAGES"
FINAL="RUNNING"
EXIT_CODE=0

stage() {  # name command...
  local name="$1"; shift
  local t0 rc t1
  t0="$(date +%s)"
  log "---- stage $name: $*"
  if "$@"; then rc=0; else rc=$?; fi
  t1="$(date +%s)"
  printf '%s\tstart=%s\tdur=%ss\trc=%s\tcmd=%s\n' "$name" "$t0" "$((t1-t0))" "$rc" "$*" >> "$STAGES"
  log "---- stage $name rc=$rc"
  return "$rc"
}
write_result() {  # 统一写 result.json
  cat > "$EVIDENCE_DIR/result.json" <<JSON
{"schema":"khaen-rt-stack/switch-result/v1","final_state":"$FINAL","exit":$EXIT_CODE,"at":"$(now_iso)","stages":"stages.log"}
JSON
}
rollback_after_retire() {  # memo: 已 retire，只允许 last-good/mute
  local rb_args=(--live --evidence "$EVIDENCE_DIR/rollback")
  [ -n "$AUTHORIZE_FILE" ] && rb_args+=(--authorize "$AUTHORIZE_FILE")
  if [ "$ALLOW_FIRST" = "1" ] || [ ! -f "$(p "$KHAEN_STATE_DIR/last-good.json")" ]; then rb_args+=(--mute); fi
  if stage 10-rollback "$HERE/08-rollback.sh" "${rb_args[@]}"; then
    FINAL="ROLLED_BACK_LAST_GOOD"; EXIT_CODE=5
  else
    local rc=$?
    if [ "$rc" = "10" ]; then FINAL="DEGRADED_MUTE"; EXIT_CODE=10; else FINAL="STOPPED_SILENT_C_MASKED"; EXIT_CODE=6; fi
  fi
  write_result
  exit "$EXIT_CODE"
}

# ---- 1 只读快照（失败=完全零变更） -------------------------------------------
if ! stage 1-snapshot "$HERE/00-snapshot-old-c.sh" --live --out "$EVIDENCE_DIR/snapshot"; then
  FINAL="ABORTED_BEFORE_CHANGES"; EXIT_CODE=3
  die "快照失败：未做任何变更" 3
fi

# ---- 2 install 前的只读门（不依赖新 release；失败=完全零变更） -----------------
if ! stage 2-preflight-pre "$HERE/01-preflight-switch.sh" --live --pre-install "${AUTH_ARGS[@]}" "${FIRST_ARGS[@]}"; then
  FINAL="ABORTED_BEFORE_CHANGES"; EXIT_CODE=3
  die "pre-install 前置门失败：未做任何变更（未安装 release/未停旧 C）" 3
fi

# ---- 3 安装 release（不 enable/不 start；落盘 + keeper enable） ----------------
if ! stage 3-install "$HERE/04-install-release.sh" --live --no-enable "${AUTH_ARGS[@]}" --bundle "$BUNDLE" --evidence "$EVIDENCE_DIR/install"; then
  FINAL="ABORTED_BEFORE_CHANGES"; EXIT_CODE=3
  die "release 安装/校验失败：未停旧 C，音频零变更（release/current 可能已部分落盘，需按证据收口）" 3
fi

# ---- 4 完整前置门（只读；release/toml/arch/192k；失败=音频零变更） -------------
if ! stage 4-preflight "$HERE/01-preflight-switch.sh" --live "${AUTH_ARGS[@]}" "${FIRST_ARGS[@]}"; then
  FINAL="ABORTED_BEFORE_CHANGES"; EXIT_CODE=3
  die "前置门失败：未停旧 C、未切换音频（install 阶段已落盘 release/current 与 keeper enable）" 3
fi

# ---- 5 退役（从这里开始不可回 C） -------------------------------------------
if ! stage 5-retire "$HERE/03-retire-c.sh" --live "${AUTH_ARGS[@]}" --evidence "$EVIDENCE_DIR/retire"; then
  FINAL="RETIRE_INCOMPLETE_C_MASKED_PARTIAL"; EXIT_CODE=6
  warn "旧 C 退役不完整：不启动新栈；按 migration-plan 失败矩阵人工收口（禁止恢复 C 服务）"
  write_result
  exit 6
fi

# ---- 6 设备独占（retire 后；失败=回滚/静音，不回 C） --------------------------
if ! stage 6-device "$HERE/02-check-device-exclusive.sh" --live; then
  FINAL="DEVICE_NOT_EXCLUSIVE_AFTER_RETIRE"; EXIT_CODE=5
  rollback_after_retire
fi

# ---- 7 启动（enable 放在 retire 之后，避免重启窗口双栈；F-18 校验 host+daemon active） --
START_FAIL=0
if ! stage 7-start bash -c "\"$KHAEN_SYSTEMCTL\" daemon-reload && \"$KHAEN_SYSTEMCTL\" enable khaen-rt-stack.target && \"$KHAEN_SYSTEMCTL\" start khaen-motu-48k-keeper.service && \"$KHAEN_SYSTEMCTL\" start khaen-irq-affinity.service && \"$KHAEN_SYSTEMCTL\" start khaen-rt-stack.target"; then
  warn "启动 khaen-rt-stack.target 失败：进入回滚（last-good/mute）"
  FINAL="START_FAILED"; EXIT_CODE=5; START_FAIL=1
elif ! wait_unit_active khaen-rt-host.service "$TIMEOUT"; then
  warn "khaen-rt-host ${TIMEOUT}s 内未 active（target Wants=未生效或 host 启动失败）：进入回滚"
  FINAL="START_TIMEOUT_HOST"; EXIT_CODE=5; START_FAIL=1
elif ! wait_unit_active khaen-rt-daemon.service "$TIMEOUT"; then
  warn "khaen-rt-daemon ${TIMEOUT}s 内未 active（T28-P0-a 要求 host+daemon 都 active）：进入回滚"
  FINAL="START_TIMEOUT_DAEMON"; EXIT_CODE=5; START_FAIL=1
fi
if [ "$START_FAIL" = "1" ]; then
  rollback_after_retire
fi

# ---- 7b IRQ 钉核证据（task-192；失败不在此中断，交给 06 verify V10 硬门/回滚） ----
if [ "$DRY_RUN" = "0" ]; then
  irq_config_load
  if py "$DEPLOY_ROOT/lib/irq_affinity.py" check --root "$ROOT" --pins "$IRQ_PINS" --t45-map "$IRQ_T45_MAP" --dsp-cpus "$IRQ_DSP_CPUS" --json "$EVIDENCE_DIR/irq-affinity.json" >"$EVIDENCE_DIR/irq-affinity.txt" 2>&1; then
    log "IRQ 钉核校验 PASS（pins: $IRQ_PINS；06 verify V10 将复验）"
  else
    warn "IRQ 钉核校验 FAIL（见 irq-affinity.txt；06 verify V10 将硬失败 → 回滚）"
  fi
fi

# ---- 8 验证 ------------------------------------------------------------------
VERIFY_ARGS=(--live --timeout "$TIMEOUT" --evidence "$EVIDENCE_DIR/verify")
[ "$REQUIRE_DAEMON" = "1" ] && VERIFY_ARGS+=(--require-daemon)
[ "$REQUIRE_RT" = "1" ] && VERIFY_ARGS+=(--require-rt)
[ "$REQUIRE_HEALTH" = "1" ] && VERIFY_ARGS+=(--require-health)
[ -n "$HEALTH_URL" ] && VERIFY_ARGS+=(--health-url "$HEALTH_URL")
if ! stage 8-verify "$HERE/06-verify-switch.sh" "${VERIFY_ARGS[@]}"; then
  warn "验证失败：进入回滚（last-good/mute）"
  FINAL="VERIFY_FAILED"; EXIT_CODE=5
  rollback_after_retire
fi

# ---- 9 last-good -------------------------------------------------------------
MARK_ARGS=(--live --evidence "$EVIDENCE_DIR/mark" --verify-report "$EVIDENCE_DIR/verify/verify-report.json")
[ -n "$AUTHORIZE_FILE" ] && MARK_ARGS+=(--authorize "$AUTHORIZE_FILE")
if ! stage 9-mark-last-good "$HERE/07-mark-last-good.sh" "${MARK_ARGS[@]}"; then
  # F-41：已验证运行但 last-good 未记录 = 独立退出码 8（不得与“已回滚”共用 5）。
  FINAL="RUNNING_NO_LAST_GOOD"; EXIT_CODE=8
  warn "新栈已运行并通过验证，但 last-good 记录失败：不中断播放，必须人工补记后才能进入 T72（无 last-good 则回退只剩 mute）"
  write_result
  exit 8
fi

FINAL="RUNNING_LAST_GOOD"; EXIT_CODE=0
write_result
log "== 05 atomic-switch 成功：$FINAL（C 保持 masked；last-good 已记录）"
exit 0
