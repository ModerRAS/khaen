#!/usr/bin/env bash
# T28 03-retire-c.sh —— 停用/禁用/防激活旧 CamillaDSP 进程链（**不可回退到 C**）
#
# 动作顺序（依据 DSP-HOST-DECISION §7 G-no-C、spec/compat-matrix C07/K13/Q38/Q04/A11）：
#   S1 旧 Web/WS shim（tspi-audio-web）stop → disable → mask
#   S2 旧编排/开机恢复入口（tspi-boot-mode / tspi-audio.target）stop → disable（target 不 mask）
#   S3 旧 C 依赖 daemon（tspi-upmix-autodetect）stop → disable → mask
#   S4 旧 C 引擎（tspi-camilladsp）stop → disable → mask
#   S5 删除旧 C 激活入口文件（/run/tspi/camilladsp.yml、camilladsp.env、/etc/tspi/last-mode、/run/tspi/mode）
#   S6 兜底 TERM/KILL 残留 camilladsp/tspi-upmix 进程并核对
#   S7 验证：C 无进程、相关 unit 非 active 且 disabled/masked、入口文件不存在、保留 AoIP/PTP 未被改动
#
# 禁止：本脚本不 enable/start 任何旧 C unit，不写 C 配置，不改内核参数/CueMix；失败即停并保留现状。
# 用法: sudo ./03-retire-c.sh --live --authorize WINDOW.json [--evidence DIR] [--root DIR]
# 退出码: 0 退役完成；3 前置/校验失败；4 授权窗口失败；5 退役不完整（已停部分，未启动新栈）；7 环境不支持。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

CLEAN_ENTRIES=1
usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --evidence) EVIDENCE_DIR="$2"; shift 2 ;;
    --keep-entry-files) CLEAN_ENTRIES=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

require_authorization "retire-c"
[ "$DRY_RUN" = "0" ] && require_root_live
evidence_prepare "03-retire-c"

FAILS=0
RETIRED=()
SKIPPED=()
notice() { log "$*"; }
fail() { FAILS=$((FAILS+1)); warn "[FAIL] $*"; }

# 记录退役前状态
if [ "$DRY_RUN" = "0" ]; then
  printf 'unit\tactive\tenabled\n' > "$EVIDENCE_DIR/pre-units.tsv"
  for u in "${ALL_OLD_AUDIO_UNITS[@]}"; do
    printf '%s\t%s\t%s\n' "$u" "$("$TSPI_SYSTEMCTL" is-active "$u" 2>/dev/null || true)" "$(sunit_enabled_state "$u")" >> "$EVIDENCE_DIR/pre-units.tsv"
  done
  pgrep -a -f 'camilladsp|tspi-upmix|tspi-audio-web' 2>/dev/null > "$EVIDENCE_DIR/pre-procs.txt" || true
  for f in "${C_ENTRY_FILES[@]}"; do
    [ -e "$(p "$f")" ] && printf '%s\t%s\n' "$f" "$(sha256_of "$(p "$f")")" >> "$EVIDENCE_DIR/pre-entry-files.tsv"
  done
fi

# F2（2026-11-18）：旧栈 overlay 把 unit 安装为 /etc/systemd/system 下的**普通文件**；
# systemctl mask 需要在该路径创建 /dev/null 符号链接，无法覆盖既有文件（板端实测
# "Failed to mask unit: File /etc/systemd/system/tspi-audio-web.service already exists."）。
# 因此 mask 前：普通文件 → 备份到 evidence/retired-unit-files/ + 移走（记录 sha256），再 mask。
# 已 masked（/dev/null 符号链接）或不存在 → 不动；目录等异常形态 → FAIL（不绕过）。
unit_file_move_before_mask() {  # unit
  local u="$1" f dst
  f="$(p "$TSPI_SYSTEMD_DIR/$u")"
  if [ -L "$f" ]; then return 0; fi   # 已 mask 的符号链接：交给 systemctl mask 幂等处理
  if [ ! -e "$f" ]; then return 0; fi
  if [ -d "$f" ]; then warn "[FAIL] $f 是目录（预期 unit 文件）；拒绝移走"; return 1; fi
  dst="$EVIDENCE_DIR/retired-unit-files"
  if [ "$DRY_RUN" = "1" ]; then
    log "[dry-run] 备份并移走普通 unit 文件后再 mask: $f -> $dst/$u（记录 sha256）"
    return 0
  fi
  mkdir -p "$dst" || { warn "[FAIL] 无法创建备份目录: $dst"; return 1; }
  cp -f "$f" "$dst/$u" || { warn "[FAIL] 备份 unit 文件失败: $f"; return 1; }
  local sha; sha="$(sha256_of "$f")"
  printf '%s\n' "$sha" > "$dst/$u.sha256" 2>/dev/null || true
  rm -f "$f" || { warn "[FAIL] 移走 unit 文件失败: $f"; return 1; }
  printf '%s\t%s\t%s\t%s\n' "$u" "$f" "$dst/$u" "$sha" >> "$EVIDENCE_DIR/moved-unit-files.tsv"
  log "已备份并移走普通 unit 文件: $f -> $dst/$u（sha256=$sha；随后 mask）"
  return 0
}

stop_disable_mask() {  # unit mask(1/0)
  local u="$1" do_mask="${2:-1}" state
  if ! sunit_exists "$u"; then SKIPPED+=("$u:NOT_PRESENT"); notice "跳过不存在 unit: $u"; return 0; fi
  if sunit_active "$u"; then run "$TSPI_SYSTEMCTL" stop "$u" || { fail "stop $u 失败"; return 1; }; fi
  state="$(sunit_enabled_state "$u")"
  if [ "$state" != "disabled" ] && [ "$state" != "masked" ]; then
    run "$TSPI_SYSTEMCTL" disable "$u" || { fail "disable $u 失败"; return 1; }
  fi
  if [ "$do_mask" = "1" ] && [ "$state" != "masked" ]; then
    unit_file_move_before_mask "$u" || { fail "mask 前移走普通 unit 文件失败: $u"; return 1; }
    run "$TSPI_SYSTEMCTL" mask "$u" || { fail "mask $u 失败"; return 1; }
  fi
  RETIRED+=("$u")
  return 0
}

log "== 03 retire-c（DRY_RUN=$DRY_RUN）"
# S1 旧 Web/WS shim 先停，防止其反向拉起/操作 C
for u in "${REPLACE_SHIM_UNITS[@]}"; do stop_disable_mask "$u" 1 || true; done
# S2 旧编排/开机恢复入口
for u in "${RETIRE_ENTRY_UNITS[@]}"; do
  if [ "$u" = "tspi-audio.target" ]; then stop_disable_mask "$u" 0 || true; else stop_disable_mask "$u" 1 || true; fi
done
# S3/S4 旧 C 链：先 WS 消费者再引擎
for u in "${RETIRE_C_UNITS[@]}"; do stop_disable_mask "$u" 1 || true; done

# S5 激活入口文件（先记录 hash 到证据，再删除；不允许后由旧 tspi-mode 复活 C）
if [ "$CLEAN_ENTRIES" = "1" ]; then
  for f in "${C_ENTRY_FILES[@]}"; do
    fp="$(p "$f")"
    [ -e "$fp" ] || continue
    if [ "$DRY_RUN" = "0" ]; then
      cp -f "$fp" "$EVIDENCE_DIR/" 2>/dev/null || true
      printf '%s\t%s\n' "$f" "$(sha256_of "$fp")" >> "$EVIDENCE_DIR/removed-entry-files.tsv"
    fi
    run rm -f "$fp" || fail "删除入口文件失败: $f"
  done
else
  notice "--keep-entry-files：保留激活入口文件（仅调试；不得作为 T71 口径）"
fi

# S6 残留进程
for pat in "${C_PROCESS_PATTERNS[@]}"; do
  pids="$(pgrep -f "$pat" 2>/dev/null || true)"
  [ -z "$pids" ] && continue
  notice "发现残留进程 pattern=$pat pids=$(printf '%s' "$pids" | tr '\n' ' ')"
  if [ "$DRY_RUN" = "0" ]; then
    printf 'TERM %s\t%s\n' "$pat" "$(printf '%s' "$pids" | tr '\n' ',')" >> "$EVIDENCE_DIR/killed-procs.txt"
    # 优先 pkill（外部命令，可按 pattern 清理；无 pkill 时按 pid 发 TERM）
    if command -v pkill >/dev/null 2>&1; then
      pkill -TERM -f "$pat" 2>/dev/null || true
    else
      # shellcheck disable=SC2086
      command kill -TERM $pids 2>/dev/null || true
    fi
    for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -f "$pat" >/dev/null 2>&1 || break; sleep "${TSPI_POLL_SECS:-1}"; done
    pids2="$(pgrep -f "$pat" 2>/dev/null || true)"
    if [ -n "$pids2" ]; then
      if command -v pkill >/dev/null 2>&1; then
        pkill -KILL -f "$pat" 2>/dev/null || true
      else
        # shellcheck disable=SC2086
        command kill -KILL $pids2 2>/dev/null || true
      fi
      printf 'KILL %s\t%s\n' "$pat" "$(printf '%s' "$pids2" | tr '\n' ',')" >> "$EVIDENCE_DIR/killed-procs.txt"
    fi
  fi
done

# S7 验证
if [ "$DRY_RUN" = "0" ]; then
  sleep "${TSPI_POLL_SECS:-1}" 2>/dev/null || true
  ACTIVE=()
  for u in "${RETIRE_C_UNITS[@]}" "${RETIRE_ENTRY_UNITS[@]}" "${REPLACE_SHIM_UNITS[@]}"; do
    sunit_active "$u" && ACTIVE+=("$u")
  done
  [ "${#ACTIVE[@]}" -eq 0 ] && notice "PASS：退役 unit 全部非 active" || fail "退役 unit 仍 active: ${ACTIVE[*]}"

  for u in "${RETIRE_C_UNITS[@]}" "${REPLACE_SHIM_UNITS[@]}"; do
    st="$(sunit_enabled_state "$u")"
    case "$st" in masked|disabled) : ;; *) fail "$u enabled=$st（要求 masked/disabled）" ;; esac
  done
  st="$(sunit_enabled_state tspi-audio.target)"
  [ "$st" = "disabled" ] && notice "PASS：tspi-audio.target disabled" || fail "tspi-audio.target enabled=$st（要求 disabled）"

  for pat in "${C_PROCESS_PATTERNS[@]}"; do
    if pgrep -f "$pat" >/dev/null 2>&1; then fail "仍有进程 pattern=$pat"; else notice "PASS：无进程 $pat"; fi
  done

  if [ "$CLEAN_ENTRIES" = "1" ]; then
    for f in "${C_ENTRY_FILES[@]}"; do [ -e "$(p "$f")" ] && fail "入口文件仍存在: $f"; done
  fi

  # 保留 AoIP/PTP：记录状态但不改动；若被旧 target 顺带停掉，这里如实记录（不自动重启）
  for u in "${RETAIN_AOIP_UNITS[@]}"; do
    printf '%s\t%s\t%s\n' "$u" "$("$TSPI_SYSTEMCTL" is-active "$u" 2>/dev/null || true)" "$(sunit_enabled_state "$u")" >> "$EVIDENCE_DIR/retained-units.tsv"
  done

  printf 'PASSES=%s FAILS=%s\nRETIRED=%s\nSKIPPED=%s\n' "$(( ${#RETIRED[@]} ))" "$FAILS" "${RETIRED[*]:-}" "${SKIPPED[*]:-}" > "$EVIDENCE_DIR/result.txt"
fi

if [ "$DRY_RUN" = "0" ] && [ "$FAILS" -gt 0 ]; then
  die "旧 C 退役不完整（$FAILS 项）；**不得启动新栈**，保留现状并按 migration-plan 失败矩阵处理（禁止恢复 C 服务）" 5
fi
log "== 03 retire-c 完成（C 已停用/禁用/防激活；未启动新栈）"
exit 0
