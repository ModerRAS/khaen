#!/usr/bin/env bash
# T28 deploy 公共库 —— deploy/bin/*.sh 全部 source 本文件。
#
# 纪律（WBS T28 + DSP-HOST-DECISION §7/§8 + rt-stack/README §3）：
#   * **默认 DRY-RUN**：只打印将执行的命令，不写盘、不做 systemctl 变更；变更脚本必须显式 --live。
#   * live 必须带 --authorize <窗口 JSON>，窗口在有效期内且 scope/audio_off_required 匹配（lib/check_window.py）。
#   * **禁止启动/使能/恢复 CamillaDSP 及其依赖 unit**（P1 G-no-C）。失败回退只允许自研 last-good 或 mute。
#   * 本套脚本不含传输层：release 由 T71 owner 用既有 deploy/ssh 凭据送到板端 staging，脚本在板端 root 执行。
#   * --root DIR 只用于本地夹具/预演（staging）；live 必须是 "/"。
#
# 运行环境：板端 Debian 12 / bash >= 5.1 / GNU coreutils / python3（窗口与 JSON 校验）。
# 本地夹具：Git Bash + PATH 前置 deploy/tests/mocks（见 tests/run_dryrun_tests.sh）。
#
# 退出码约定（各脚本一致）：
#   0 OK / 已是目标状态；2 用法或缺少依赖文件；3 前置门失败（未做任何变更）；
#   4 授权窗口缺失/过期/scope 不符；5 切换或验证失败（已尝试回滚）；6 回滚失败（静音/停止，C 保持 masked）；
#   7 环境不支持（非 root、缺命令、非 Linux 板端）；10 rollback 降级为 mute（服务在但静音）。

set -uo pipefail

# ---------------------------------------------------------------------------
# 路径（全部可被环境变量覆盖；--root 只影响文件系统访问，不影响 systemctl）
# ---------------------------------------------------------------------------
KHAEN_DEPLOY_ROOT="${KHAEN_DEPLOY_ROOT:-/}"
KHAEN_STACK_PREFIX="${KHAEN_STACK_PREFIX:-/opt/khaen-rt-stack}"
KHAEN_ETC="${KHAEN_ETC:-/etc/khaen-rt-stack}"
KHAEN_STATE_DIR="${KHAEN_STATE_DIR:-/var/lib/khaen-rt-stack}"
KHAEN_RUN_DIR="${KHAEN_RUN_DIR:-/run/khaen-rt-stack}"
KHAEN_LOG_DIR="${KHAEN_LOG_DIR:-/var/log/khaen-rt-stack}"
KHAEN_EVIDENCE_BASE="${KHAEN_EVIDENCE_BASE:-/var/lib/khaen-rt-stack/evidence}"
KHAEN_SYSTEMD_DIR="${KHAEN_SYSTEMD_DIR:-/etc/systemd/system}"
KHAEN_SYSTEMCTL="${KHAEN_SYSTEMCTL:-systemctl}"
KHAEN_PYTHON="${KHAEN_PYTHON:-}"
KHAEN_TIMEOUT_SECS="${KHAEN_TIMEOUT_SECS:-120}"

DRY_RUN="${KHAEN_DEPLOY_DRY_RUN:-1}"
ROOT="${KHAEN_DEPLOY_ROOT:-/}"
AUTHORIZE_FILE="${KHAEN_AUTHORIZE_FILE:-}"
EVIDENCE_DIR="${KHAEN_EVIDENCE_RUN:-}"
RUN_TS="$(date +%Y%m%dT%H%M%S)"
# 注意：不要占用调用方的 HERE（各脚本自己设置）；本库只用 COMMON_DIR。
COMMON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_ROOT="$(cd "$COMMON_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# 日志
# ---------------------------------------------------------------------------
ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
log() { printf '[deploy %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
warn() { printf '[deploy %s][warn] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
die() { local code="${2:-1}"; printf '[deploy %s][error] %s\n' "$(date +%H:%M:%S)" "$1" >&2; exit "$code"; }

# 文件系统路径前缀：/etc/x -> "$ROOT/etc/x"；live 时 ROOT=/ 原样返回。
p() {
  local x="$1"
  if [ "$ROOT" = "/" ] || [ -z "$ROOT" ]; then printf '%s' "$x"; else printf '%s%s' "${ROOT%/}" "$x"; fi
}

# ---------------------------------------------------------------------------
# 执行包装
# ---------------------------------------------------------------------------
# run: dry-run 只打印；live 执行并保留退出码。
run() {
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] $*"; return 0; fi
  log "+ $*"; "$@"
}
# run_sh: 需要管道/重定向的字符串命令（由调用方负责转义）。
run_sh() { run bash -c "$*"; }
# run_ro: 只读命令；dry-run 也不执行（只打印），保证夹具/预演零副作用。
run_ro() {
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run][ro] $*"; return 0; fi
  "$@"
}
need_cmd() {
  command -v "$1" >/dev/null 2>&1 && return 0
  warn "缺少命令 $1；该项按 UNSUPPORTED 记录（不绕过、不假装通过）"
  return 1
}
py() {
  if [ -n "$KHAEN_PYTHON" ]; then "$KHAEN_PYTHON" "$@"; return $?; fi
  # 注意：Windows 上 python3 可能是 Microsoft Store 占位 stub（command -v 成功但不可用），必须实测。
  if command -v python3 >/dev/null 2>&1 && python3 -c "import sys" >/dev/null 2>&1; then python3 "$@"; return $?; fi
  if command -v python >/dev/null 2>&1 && python -c "import sys" >/dev/null 2>&1; then python "$@"; return $?; fi
  die "需要可用的 python3/python（可用 KHAEN_PYTHON 指定）" 7
}
sha256_of() { sha256sum -- "$1" 2>/dev/null | awk '{print $1}'; }
file_size_of() { stat -c '%s' -- "$1" 2>/dev/null || printf '0'; }

# ---------------------------------------------------------------------------
# 前置门
# ---------------------------------------------------------------------------
require_root_live() {
  [ "$DRY_RUN" = "1" ] && return 0
  if [ "$ROOT" != "/" ]; then
    warn "ROOT=$ROOT（staging/夹具模式）：跳过 root 检查，仅用于本地预演"
    return 0
  fi
  [ "$(id -u)" = "0" ] || die "live 需要 root（uid=$(id -u)）；只做检查请保持默认 dry-run" 7
}

# 校验授权窗口（live 必须；dry-run 跳过）。$1 = 逗号分隔 scope
require_authorization() {
  local scope="$1"
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] 跳过授权窗口校验（scope=$scope；不会发生任何变更）"; return 0; fi
  [ -n "$AUTHORIZE_FILE" ] || die "live 必须提供 --authorize <窗口 JSON>（scope=$scope）" 4
  [ -r "$AUTHORIZE_FILE" ] || die "窗口文件不可读: $AUTHORIZE_FILE" 4
  py "$DEPLOY_ROOT/lib/check_window.py" "$AUTHORIZE_FILE" "$scope" || exit 4
}

# 是否已进入过渡态（旧 C 激活入口仍在 => 未退役）
c_entry_present() {
  local f
  for f in /run/khaen/camilladsp.yml /run/khaen/camilladsp.env /etc/khaen/last-mode; do
    [ -e "$(p "$f")" ] && { printf '%s\n' "$f"; return 0; }
  done
  return 1
}

# ---------------------------------------------------------------------------
# systemd 查询（只读；夹具下由 mocks/systemctl 提供）
# ---------------------------------------------------------------------------
sunit_active() { "$KHAEN_SYSTEMCTL" is-active --quiet "$1" >/dev/null 2>&1; }
sunit_enabled_state() { "$KHAEN_SYSTEMCTL" is-enabled "$1" 2>/dev/null || true; }
sunit_exists() { "$KHAEN_SYSTEMCTL" cat "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------------------
# IRQ 亲和持久化配置（task-192；/etc/khaen-rt-stack/irq-affinity.env）
# ---------------------------------------------------------------------------
# 默认值来自 task-189 板端实测（evidence/board-irq-affinity/）：IRQ58(xhci/MOTU)→CPU1、
# IRQ56(end0/PTP)→CPU2、IRQ57(end0 第二队列)→CPU3；DSP_CPUS={4,5,6,7}。
# 调用后设置 IRQ_PINS / IRQ_T45_MAP / IRQ_DSP_CPUS；配置文件存在时覆盖默认（env 变量优先）。
irq_config_load() {
  IRQ_PINS="${KHAEN_IRQ_PINS:-58:1 56:2 57:3}"
  IRQ_T45_MAP="${KHAEN_IRQ_T45_MAP:-usb=58,ptp=56,eth=56}"
  IRQ_DSP_CPUS="${KHAEN_IRQ_DSP_CPUS:-4,5,6,7}"
  local ef line key val
  ef="$(p "$KHAEN_ETC/irq-affinity.env")"
  [ -f "$ef" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ""|\#*|\;*) continue ;; esac
    key="${line%%=*}"; val="${line#*=}"
    val="${val%\"}"; val="${val#\"}"
    case "$key" in
      KHAEN_IRQ_PINS) [ -n "${KHAEN_IRQ_PINS:-}" ] || IRQ_PINS="$val" ;;
      KHAEN_IRQ_T45_MAP) [ -n "${KHAEN_IRQ_T45_MAP:-}" ] || IRQ_T45_MAP="$val" ;;
      KHAEN_IRQ_DSP_CPUS) [ -n "${KHAEN_IRQ_DSP_CPUS:-}" ] || IRQ_DSP_CPUS="$val" ;;
    esac
  done < "$ef"
}

# ---------------------------------------------------------------------------
# 证据目录
# ---------------------------------------------------------------------------
# evidence_prepare：设置 evidence 目录（若调用方未给）并在 live 时创建。
evidence_prepare() {
  local name="$1"
  [ -n "$EVIDENCE_DIR" ] || EVIDENCE_DIR="${KHAEN_EVIDENCE_RUN:-$(p "$KHAEN_EVIDENCE_BASE")/${RUN_TS}-${name}}"
  if [ "$DRY_RUN" = "1" ]; then
    log "[dry-run] evidence 目录（不会创建）: $EVIDENCE_DIR"
  else
    mkdir -p "$EVIDENCE_DIR" || die "无法创建证据目录: $EVIDENCE_DIR" 7
    log "evidence: $EVIDENCE_DIR"
  fi
}

evidence_begin() {  # $1 = 名称（如 03-retire-c）
  local name="$1"
  EVIDENCE_DIR="${KHAEN_EVIDENCE_RUN:-$(p "$KHAEN_EVIDENCE_BASE")/${RUN_TS}-${name}}"
  if [ "$DRY_RUN" = "1" ]; then
    log "[dry-run] evidence 目录（不会创建）: $EVIDENCE_DIR"
  else
    mkdir -p "$EVIDENCE_DIR" || die "无法创建证据目录: $EVIDENCE_DIR" 7
    log "evidence: $EVIDENCE_DIR"
  fi
}
ev_write() {  # $1 = 文件名，其余为内容（单行）
  local name="$1"; shift
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] evidence $name"; return 0; fi
  printf '%s\n' "$*" > "$EVIDENCE_DIR/$name"
}
ev_append() {
  local name="$1"; shift
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] evidence >> $name"; return 0; fi
  printf '%s\n' "$*" >> "$EVIDENCE_DIR/$name"
}
ev_cmd() {  # 执行并记录到 commands.log（时间/exit/命令）；返回真实退出码（T28-P1-a：不再吞错）
  local rc=0
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] $*"; return 0; fi
  log "+ $*"
  # 注意：不能写 if ! "$@"; then rc=$?；取反后的 $? 恒为 0，会把失败当成功（T28-P1-a）。
  if "$@"; then rc=0; else rc=$?; fi
  printf '%s\texit=%s\t%s\n' "$(ts)" "$rc" "$*" >> "$EVIDENCE_DIR/commands.log"
  return "$rc"
}

# ---------------------------------------------------------------------------
# 新栈路径
# ---------------------------------------------------------------------------
# current release 解析：优先 Linux 符号链接；Windows/无符号链接权限的夹具用 current.release 指针文件。
current_release_dir() {
  local cur ptr
  cur="$(p "$KHAEN_STACK_PREFIX/current")"
  ptr="$(p "$KHAEN_STACK_PREFIX/current.release")"
  if [ -L "$cur" ]; then readlink -f "$cur" 2>/dev/null && return 0; fi
  if [ -f "$ptr" ]; then tr -d "\r\n" < "$ptr"; return 0; fi
  if [ -d "$cur" ]; then printf "%s" "$cur"; return 0; fi
  return 1
}
# set_current：原子切换 current（Linux 用 symlink；不可用时降级为指针文件，并打印实际模式）
set_current() {
  local rel="$1" cur ptr tmp mode
  cur="$(p "$KHAEN_STACK_PREFIX/current")"
  ptr="$(p "$KHAEN_STACK_PREFIX/current.release")"
  if ln -sfn "$rel" "$cur.tmp" 2>/dev/null && [ -L "$cur.tmp" ]; then
    mv -Tf "$cur.tmp" "$cur" || return 1
    rm -f "$ptr" 2>/dev/null || true
    printf "symlink"
    return 0
  fi
  rm -f "$cur.tmp" 2>/dev/null || true
  if [ -L "$cur" ] || [ -f "$cur" ]; then rm -f "$cur" 2>/dev/null || true; fi
  tmp="$ptr.tmp.$"
  printf "%s\n" "$rel" > "$tmp" || return 1
  mv -f "$tmp" "$ptr" || return 1
  printf "pointer"
  return 0
}
release_bin() { printf '%s/%s' "$(current_release_dir 2>/dev/null)" "$1"; }

# ---------------------------------------------------------------------------
# 工具
# ---------------------------------------------------------------------------
now_iso() { date '+%Y-%m-%dT%H:%M:%S%z'; }

# ---------------------------------------------------------------------------
# 等待 / C 退役断言
# ---------------------------------------------------------------------------
# wait_unit_active unit [timeout_secs]：反复 is-active 直到成功/超时。
wait_unit_active() {
  local u="$1" timeout="${2:-$KHAEN_TIMEOUT_SECS}" i=0
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] 等待 $u active（<= ${timeout}s）"; return 0; fi
  while [ "$i" -lt "$timeout" ]; do
    sunit_active "$u" && return 0
    sleep "${KHAEN_POLL_SECS:-1}"
    i=$((i+1))
  done
  return 1
}
# assert_c_retired：输出违规项（旧 C unit 未 masked/active、C 进程），返回 0=干净。
# F-42 修复（2026-11-17）：启用态只查 enabled 不够——disabled 且未 mask 仍可能被依赖/入口拉起。
#   必须 masked：C 进程链 + 旧 web/WS shim + boot-mode；khaen-audio.target 不能 mask，只要求非 enabled。
# 依赖调用方已 source config/old-c-units.conf（RETIRE_*_UNITS / C_PROCESS_PATTERNS）。
assert_c_retired() {
  local u st bad=0
  local mask_required=("${RETIRE_C_UNITS[@]}" "${REPLACE_SHIM_UNITS[@]}")
  for u in "${RETIRE_ENTRY_UNITS[@]}"; do
    [ "$u" = "khaen-audio.target" ] || mask_required+=("$u")
  done
  for u in "${RETIRE_C_UNITS[@]}" "${RETIRE_ENTRY_UNITS[@]}" "${REPLACE_SHIM_UNITS[@]}"; do
    st="$(sunit_enabled_state "$u")"
    case "$st" in
      enabled|enabled-runtime) printf '%s(enabled=%s) ' "$u" "$st"; bad=1 ;;
    esac
    if sunit_active "$u"; then printf '%s(active) ' "$u"; bad=1; fi
  done
  for u in "${mask_required[@]}"; do
    st="$(sunit_enabled_state "$u")"
    if [ "$st" != "masked" ]; then printf '%s(not-masked=%s) ' "$u" "${st:-unknown}"; bad=1; fi
  done
  for p in "${C_PROCESS_PATTERNS[@]}"; do
    if pgrep -f "$p" >/dev/null 2>&1; then printf 'process:%s ' "$p"; bad=1; fi
  done
  [ "$bad" = "0" ] || printf '\n'
  return "$bad"
}
# ---------------------------------------------------------------------------
# F5（2026-11-18）：unit 运行时环境核对
#   旧实现只用 `systemctl show -p Environment`，该属性**不包含 EnvironmentFile 内容**，
#   而 08-rollback 的 mute 正是经 drop-in 的 EnvironmentFile=90-safe-mute.env 注入 →
#   板端实测误报 NOT_APPLIED（假阴性，见 evidence/board-window-20261118/deploy/FINDINGS.md F5）。
#   现行顺序：
#     1) /proc/<MainPID>/environ（进程真实环境；需 unit active）
#     2) systemctl show -p EnvironmentFiles --value，逐文件解析（覆盖 drop-in 追加项）
#     3) 兜底 systemctl show -p Environment（只含 Environment=，老 systemd/夹具）
#   KHAEN_PROC_ROOT 只用于 mock/夹具（默认 /proc；夹具下由 ROOT 前缀拼接）。
# ---------------------------------------------------------------------------
KHAEN_PROC_ROOT="${KHAEN_PROC_ROOT:-/proc}"

# unit_main_pid unit -> MainPID（非数字/0/空 => 空串）
unit_main_pid() {
  local pid
  pid="$("$KHAEN_SYSTEMCTL" show --property=MainPID --value "$1" 2>/dev/null | tr -d '[:space:]')" || true
  case "$pid" in ''|0|*[!0-9]*) printf '' ;; *) printf '%s' "$pid" ;; esac
}

# _proc_environ_contains pid needle -> 0/1
_proc_environ_contains() {
  local pid="$1" needle="$2" f
  [ -n "$pid" ] || return 1
  f="$(p "$KHAEN_PROC_ROOT/$pid/environ")"
  [ -r "$f" ] || return 1
  tr '\0' '\n' < "$f" 2>/dev/null | grep -qxF -- "$needle"
}

# unit_environment_files unit -> 换行分隔的 EnvironmentFile 路径（去掉 systemd 的 '-' 前缀）
unit_environment_files() {
  local u="$1" files f
  files="$("$KHAEN_SYSTEMCTL" show --property=EnvironmentFiles --value "$u" 2>/dev/null || true)"
  for f in $files; do
    f="${f#-}"
    [ -n "$f" ] || continue
    case "$f" in /*) printf '%s\n' "$f" ;; *) printf '%s\n' "$f" ;; esac
  done
}

# _env_file_contains file needle -> 0/1（KEY=VALUE 行；注释/空行跳过）
_env_file_contains() {
  local f="$1" needle="$2"
  [ -r "$f" ] || return 1
  sed -n 's/^[[:space:]]*//; s/[[:space:]]*$//; /^#/d; /^;/d; /^$/d; p' "$f" 2>/dev/null | grep -qxF -- "$needle"
}

# unit_env_contains unit 'KEY=VALUE'：unit 的**有效运行时环境**是否含该字面赋值
unit_env_contains() {
  local u="$1" needle="$2" pid f envs
  # 1) 真实进程环境（systemd ExecStart 把 Environment= 与 EnvironmentFile= 合并后传给进程）
  pid="$(unit_main_pid "$u")"
  if _proc_environ_contains "$pid" "$needle"; then return 0; fi
  # 2) EnvironmentFiles（systemctl 可从 unit+drop-in 解析得到；无需读取 /proc）
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if _env_file_contains "$(p "$f")" "$needle"; then return 0; fi
  done < <(unit_environment_files "$u")
  # 3) 兜底：Environment=（不含 EnvironmentFile；仅老 systemd/夹具）
  envs="$("$KHAEN_SYSTEMCTL" show --property=Environment "$u" 2>/dev/null || true)"
  case "$envs" in *"$needle"*) return 0 ;; esac
  return 1
}

# unit_env_verify_source unit -> 描述实际生效的核对通道（证据用）
unit_env_verify_source() {
  local u="$1" pid f
  pid="$(unit_main_pid "$u")"
  if [ -n "$pid" ] && [ -r "$(p "$KHAEN_PROC_ROOT/$pid/environ")" ]; then printf '/proc/%s/environ' "$pid"; return 0; fi
  f="$(unit_environment_files "$u" | head -n 1)"
  if [ -n "$f" ]; then printf 'EnvironmentFiles(%s)' "$f"; return 0; fi
  printf 'systemctl-show-Environment(fallback)'
}

