#!/usr/bin/env bash
# T28 04-install-release.sh —— 安装自研 release bundle（二进制/unit/env 模板/current 软链）
#
# 不改运行状态：只落盘 + daemon-reload + enable（不 start）；start 由 05-atomic-switch.sh 负责。
# bundle 由 deploy/bin/make-release-bundle.sh 生成（manifest.json 绑定 sha256/arch，见 lib/release_manifest.py）。
#
# 安装布局：
#   /opt/khaen-rt-stack/releases/<release_id>/bin/{khaen-rt-host,...}
#   /opt/khaen-rt-stack/releases/<release_id>/units/*.service|*.target
#   /opt/khaen-rt-stack/releases/<release_id>/manifest.json, README.deploy.md
#   /opt/khaen-rt-stack/current -> releases/<release_id>（原子替换）
#   /etc/systemd/system/khaen-rt-*.service|target（0644）
#   /etc/khaen-rt-stack/rt-host.env / rt-daemon.env / rt-host.toml / rt-daemon.toml
#     （仅当不存在时从模板复制，不覆盖既有；F-18/T28-P0-2：缺失 toml 时 host/daemon 启动即退）
#   /etc/khaen-rt-stack/irq-affinity.env（task-192；仅首次安装，不覆盖）
#   /usr/local/lib/khaen/irq-affinity-pin（task-192；由 khaen-irq-affinity.service oneshot 调用）
#
# task-192 IRQ 亲和持久化（T71 前置；task-189 实测 irqbalance 会把 IRQ58/56/57 搬回 A72）：
#   * 安装 pin 脚本 + irq-affinity.env +（bundle 内的）khaen-irq-affinity.service；
#   * **disable + stop irqbalance**（等价 systemctl disable --now irqbalance），记录原状态；
#   * 立即按 pins 写 /proc/irq/<irq>/smp_affinity_list 并回读校验（失败 exit 3：旧 C 运行态未动，
#     属音频零变更，见 05 失败矩阵），并由 lib/irq_affinity.py 做 T45 IRQ 放置前置检查 + 证据 JSON。
#
# B1（2026-11-20 板窗 release-v11 实测）：开机 ~4s oneshot 早于 end0 IRQ56/57 节点就绪，apply fail-closed
#   → host 因 Requires= 不启动。修复固化到本脚本安装路径：
#     * bundle 的 khaen-irq-affinity.service 必须含 ExecStartPre=irq-affinity-pin wait（安装前校验，缺则 exit 3 不改板）；
#     * pin 脚本必须支持 wait 动作；安装后再次确认已装 unit 含该前置；
#     * 板端存量 live-only drop-in（khaen-irq-affinity.service.d/10-wait-irq.conf）迁移到
#       evidence/migrated-dropins/（备份 sha256）后删除，避免陈旧硬编码等待与新 unit 内置等待叠加。
#
# 用法: sudo ./04-install-release.sh --live --authorize WINDOW.json --bundle /var/tmp/khaen-rt-stack-release-<id>
# 退出码: 0 安装完成/已安装；2 用法/缺 bundle；3 校验失败（未安装）；4 授权失败；7 环境不支持。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

BUNDLE=""
ALLOW_FOREIGN=0
NO_ENABLE=0
usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle) BUNDLE="$2"; shift 2 ;;
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --evidence) EVIDENCE_DIR="$2"; shift 2 ;;
    --allow-foreign-arch) ALLOW_FOREIGN=1; shift ;;
    --no-enable) NO_ENABLE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

require_authorization "install-release"
[ "$DRY_RUN" = "0" ] && require_root_live
evidence_prepare "04-install-release"

if [ -z "$BUNDLE" ]; then
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] 未给 --bundle：将只校验现有 current release 并打印安装动作"; else
    die "缺少 --bundle（release bundle 目录，含 manifest.json）" 2
  fi
fi

STACK_DIR="$(p "$KHAEN_STACK_PREFIX")"
REL_ROOT="$STACK_DIR/releases"
SYSD_DIR="$(p "$KHAEN_SYSTEMD_DIR")"
REL_ID=""
REL_DIR=""
PREV_CURRENT=""

# ---- 校验 bundle（读取；dry-run 也执行只读校验，若 bundle 存在） -------------------------
if [ -n "$BUNDLE" ] && [ -d "$BUNDLE" ]; then
  log "校验 release bundle: $BUNDLE"
  VERIFY_ARGS=()
  [ "$ALLOW_FOREIGN" = "1" ] || VERIFY_ARGS+=(--require-arch aarch64)
  if out="$(py "$DEPLOY_ROOT/lib/release_manifest.py" verify "$BUNDLE" "${VERIFY_ARGS[@]}" 2>&1)"; then
    REL_ID="$(printf '%s' "$out" | py -c 'import json,sys;print(json.load(sys.stdin)["release_id"])')"
    log "bundle PASS：release_id=$REL_ID"
  else
    if [ "$DRY_RUN" = "1" ] && [ "$ALLOW_FOREIGN" = "0" ]; then
      warn "[dry-run] bundle 校验未通过（未安装）：$out"
      log "[dry-run] 若为本地测试夹具需要 --allow-foreign-arch；真实产物必须是 ELF aarch64"
      exit 0
    fi
    die "release bundle 校验失败（未安装）：$out" 3
  fi
elif [ -n "$BUNDLE" ]; then
  if [ "$DRY_RUN" = "1" ]; then warn "[dry-run] bundle 不存在: $BUNDLE（仅打印安装动作）"; else die "bundle 不存在: $BUNDLE" 2; fi
fi

if [ -z "$REL_ID" ]; then
  REL_ID="${KHAEN_RELEASE_ID:-unknown-local}"
fi
REL_DIR="$REL_ROOT/$REL_ID"
CUR="$(p "$KHAEN_STACK_PREFIX/current")"
PREV_CURRENT="$(readlink -f "$CUR" 2>/dev/null || true)"

log "release: $REL_DIR（安装布局: bin/ units/；previous_current=${PREV_CURRENT:-none}）"
if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] install -d -m 0755 $REL_DIR/bin $REL_DIR/units"
  log "[dry-run] install -m 0755 $BUNDLE/bin/* $REL_DIR/bin/"
  log "[dry-run] install -m 0644 $BUNDLE/units/* $SYSD_DIR/（.service/.target；不 start）"
  log "[dry-run] cp -f $BUNDLE/README.deploy.md $REL_DIR/"
  log "[dry-run] ln -sfn $REL_DIR $CUR.tmp && mv -Tf $CUR.tmp $CUR"
  log "[dry-run] 首次安装 config 模板: deploy/config/{rt-host,rt-daemon}.{env,toml}.example + irq-affinity.env.example -> $(p "$KHAEN_ETC")/（不覆盖既有）"
  log "[dry-run] IRQ 亲和持久化（task-192）：install -m0755 deploy/config/irq-affinity-pin -> $(p /usr/local/lib/khaen)/irq-affinity-pin"
  log "[dry-run] IRQ 亲和持久化：systemctl disable+stop irqbalance（记录原 enabled/active；等价 disable --now）"
  log "[dry-run] IRQ 亲和持久化：irq-affinity-pin apply（写 /proc/irq/{58,56,57}/smp_affinity_list=1/2/3 并回读），lib/irq_affinity.py check -> evidence/irq-affinity.json"
  log "[dry-run] B1 开机竞态防护：安装前校验 bundle units/khaen-irq-affinity.service 含 ExecStartPre=irq-affinity-pin wait + pin 脚本含 wait 动作（缺则 exit 3 不改板）"
  log "[dry-run] B1 开机竞态防护：迁移 legacy live-only drop-in $SYSD_DIR/khaen-irq-affinity.service.d/10-wait-irq.conf -> evidence/migrated-dropins/（备份 sha256 后删除）"
  log "[dry-run] MOTU 48k keeper: install -m0755 deploy/config/motu-48k-keeper -> $(p /usr/local/lib/khaen)/motu-48k-keeper"
  log "[dry-run] MOTU 48k keeper: install unit/timer -> $SYSD_DIR；udev rule -> $(p /etc/udev/rules.d)/95-khaen-motu-48k-keeper.rules"
  log "[dry-run] MOTU 48k keeper v2：install -m0644 deploy/config/motu-rate-guard.env -> $(p /etc/khaen)/motu-rate-guard.env（不覆盖既有；WRITE_BACK 默认 0）"
  log "[dry-run] systemctl daemon-reload; systemctl enable khaen-rt-stack.target khaen-motu-48k-keeper.service khaen-motu-48k-keeper.timer"
  exit 0
fi

# ---- B1 开机 IRQ 竞态防护（2026-11-20 板窗）：安装前校验 bundle（失败不改板） --------
if [ -n "$BUNDLE" ] && [ -d "$BUNDLE" ]; then
  if [ ! -f "$BUNDLE/units/khaen-irq-affinity.service" ]; then
    die "release bundle 缺 khaen-irq-affinity.service（task-192/B1 IRQ 持久化；用现行 service/ 重新 make-release-bundle.sh）" 3
  fi
  if ! grep -q '^ExecStartPre=/usr/local/lib/khaen/irq-affinity-pin wait' "$BUNDLE/units/khaen-irq-affinity.service"; then
    die "B1 开机竞态防护缺失：bundle 的 khaen-irq-affinity.service 无 ExecStartPre=irq-affinity-pin wait（重启后 IRQ56/57 晚到会使 host 不启动；用现行 service/ 重打 bundle）" 3
  fi
  if ! grep -q 'irq-affinity-pin wait' "$DEPLOY_ROOT/config/irq-affinity-pin"; then
    die "B1 开机竞态防护缺失：deploy/config/irq-affinity-pin 无 wait 动作（deploy 文件不完整）" 3
  fi
fi

# ---- 安装 --------------------------------------------------------------------
mkdir -p "$REL_DIR/bin" "$REL_DIR/units" "$SYSD_DIR" "$(p "$KHAEN_ETC")" "$(p "$KHAEN_STATE_DIR")" || die "无法创建安装目录" 7
if [ -n "$BUNDLE" ] && [ -d "$BUNDLE" ]; then
  ev_cmd install -d -m 0755 "$REL_DIR/bin" "$REL_DIR/units" "$REL_DIR/etc" || true
  for f in "$BUNDLE"/bin/*; do
    [ -f "$f" ] || continue
    ev_cmd install -m 0755 "$f" "$REL_DIR/bin/" || die "安装 bin 失败: $f" 3
  done
  for f in "$BUNDLE"/units/*.service "$BUNDLE"/units/*.target; do
    [ -f "$f" ] || continue
    ev_cmd install -m 0644 "$f" "$SYSD_DIR/" || die "安装 unit 失败: $f" 3
  done
  # task-192：IRQ 持久化 oneshot 必须随 bundle 安装；旧 bundle 缺它则拒绝安装（须重新 make-release-bundle）
  if [ ! -f "$SYSD_DIR/khaen-irq-affinity.service" ]; then
    die "release bundle 缺 khaen-irq-affinity.service（task-192 IRQ 持久化；用现行 service/ 重新 make-release-bundle.sh）" 3
  fi
  # B1（2026-11-20 板窗）：安装后的 oneshot 必须含开机等待前置（bundle 已在安装前校验；此处复核落盘结果）
  if ! grep -q '^ExecStartPre=/usr/local/lib/khaen/irq-affinity-pin wait' "$SYSD_DIR/khaen-irq-affinity.service"; then
    die "B1 开机竞态防护缺失：已安装的 khaen-irq-affinity.service 无 ExecStartPre=irq-affinity-pin wait" 3
  fi
  for f in "$BUNDLE"/units/* "$BUNDLE"/etc/*; do
    [ -f "$f" ] || continue
    case "$f" in
      *"/units/"*) ev_cmd install -m 0644 "$f" "$REL_DIR/units/" || die "复制 release units 失败: $f" 7 ;;
      *"/etc/"*) ev_cmd install -m 0640 "$f" "$REL_DIR/etc/" || die "复制 release etc 失败: $f" 7 ;;
    esac
  done
  [ -f "$BUNDLE/README.deploy.md" ] && ev_cmd install -m 0644 "$BUNDLE/README.deploy.md" "$REL_DIR/README.deploy.md" || true
  [ -f "$BUNDLE/manifest.json" ] && ev_cmd install -m 0644 "$BUNDLE/manifest.json" "$REL_DIR/manifest.json" || true
fi
# release 目录内必须能独立校验（manifest 相对当前目录）
if [ ! -f "$REL_DIR/manifest.json" ]; then
  # 已是同 release 的二次安装（current 指向它）或 bundle 校验缺失：用现有 manifest
  if [ -f "$(readlink -f "$CUR" 2>/dev/null || true)/manifest.json" ]; then
    cp -f "$(readlink -f "$CUR")/manifest.json" "$REL_DIR/manifest.json"
  else
    die "release 目录缺 manifest.json（bundle 不完整）；不切换 current" 3
  fi
fi

# 原子切换 current（Linux symlink；Windows 夹具降级 pointer file）
CURRENT_MODE="$(set_current "$REL_DIR")" || die "原子切换 current 失败" 7
log "current 切换模式：$CURRENT_MODE（board 生产应为 symlink）"

# config 模板（environ + toml；仅当不存在；F-18/T28-P0-2 补 toml 安装）
ETC_DIR="$(p "$KHAEN_ETC")"
for pair in "rt-host.env:rt-host.env.example" "rt-daemon.env:rt-daemon.env.example" \
            "rt-host.toml:rt-host.toml.example" "rt-daemon.toml:rt-daemon.toml.example" \
            "irq-affinity.env:irq-affinity.env.example"; do
  dst="$ETC_DIR/${pair%%:*}"; src="$DEPLOY_ROOT/config/${pair##*:}"
  if [ -f "$dst" ]; then
    log "保留既有配置（不覆盖）: $dst"
  elif [ -f "$src" ]; then
    ev_cmd install -m 0640 "$src" "$dst" || die "写配置失败: $dst（缺 toml 时 host/daemon 启动即退）" 3
  else
    die "缺少配置模板: $src" 3
  fi
done

# ---- IRQ 亲和持久化（task-192；T71 前置：T45 依赖 IRQ 必须离开 A72 DSP 核） ----
# 依据：task-189 实测 IRQ58→CPU1、IRQ56→CPU2、IRQ57→CPU3 后 T45 plan accepted；
#       irqbalance 重启 ≤25s 会搬回 A72 → 这里 disable+stop，并由 oneshot 在 host 启动前再次钉核。
IRQ_LIB="$(p /usr/local/lib/khaen)"
IRQ_PIN="$IRQ_LIB/irq-affinity-pin"
irq_config_load
if [ -f "$DEPLOY_ROOT/config/irq-affinity-pin" ]; then
  ev_cmd install -d -m 0755 "$IRQ_LIB" || die "创建 $IRQ_LIB 失败" 7
  ev_cmd install -m 0755 "$DEPLOY_ROOT/config/irq-affinity-pin" "$IRQ_PIN" || die "安装 irq-affinity-pin 失败（IRQ 持久化不可用）" 3
else
  die "缺少 deploy/config/irq-affinity-pin（task-192 IRQ 持久化不可用）" 3
fi
# B1（2026-11-20 板窗）：pin 脚本必须支持 wait 动作（unit ExecStartPre 依赖）；--help 不产生副作用。
if ! "$IRQ_PIN" --help 2>&1 | grep -q 'irq-affinity-pin wait'; then
  die "B1 开机竞态防护缺失：$IRQ_PIN 不支持 wait 动作（unit ExecStartPre 会失败；用现行 deploy/config/irq-affinity-pin 重装）" 3
fi

# B1（2026-11-20 板窗）：迁移板端存量 live-only drop-in（10-wait-irq.conf）——其语义已由新 unit 的
# ExecStartPre 内置；保留会叠加陈旧硬编码 IRQ 等待（IRQ 号变化时先超时失败，反而挡住新 unit）。
LEGACY_DROPIN="$SYSD_DIR/khaen-irq-affinity.service.d/10-wait-irq.conf"
LEGACY_STATE="absent"
if [ -f "$LEGACY_DROPIN" ]; then
  ev_cmd install -d -m 0755 "$EVIDENCE_DIR/migrated-dropins" || die "创建 migrated-dropins 证据目录失败" 7
  ev_cmd cp -p "$LEGACY_DROPIN" "$EVIDENCE_DIR/migrated-dropins/10-wait-irq.conf" || die "备份 legacy live-only drop-in 失败" 3
  printf '%s  10-wait-irq.conf\n' "$(sha256_of "$EVIDENCE_DIR/migrated-dropins/10-wait-irq.conf")" \
    > "$EVIDENCE_DIR/migrated-dropins/SHA256SUMS" || die "写 legacy drop-in SHA256SUMS 失败" 7
  ev_cmd rm -f "$LEGACY_DROPIN" || die "移除 legacy live-only drop-in 失败（新旧等待叠加会引入重启风险）" 3
  rmdir "$SYSD_DIR/khaen-irq-affinity.service.d" 2>/dev/null || true
  LEGACY_STATE="migrated"
  log "B1：legacy live-only drop-in 已迁移到 $EVIDENCE_DIR/migrated-dropins/ 并删除（新 unit 已内置 wait）"
fi
RB_BEFORE_ENABLED="absent"; RB_BEFORE_ACTIVE="absent"
if sunit_exists "$IRQ_BALANCE_UNIT"; then
  RB_BEFORE_ENABLED="$(sunit_enabled_state "$IRQ_BALANCE_UNIT")"
  if sunit_active "$IRQ_BALANCE_UNIT"; then RB_BEFORE_ACTIVE="active"; else RB_BEFORE_ACTIVE="inactive"; fi
  # 等价 systemctl disable --now irqbalance（拆成两步便于夹具 mock 与证据分列）
  ev_cmd "$KHAEN_SYSTEMCTL" disable "$IRQ_BALANCE_UNIT" || die "disable $IRQ_BALANCE_UNIT 失败（T45 依赖 IRQ 会被搬回 A72；不切换）" 3
  if sunit_active "$IRQ_BALANCE_UNIT"; then
    ev_cmd "$KHAEN_SYSTEMCTL" stop "$IRQ_BALANCE_UNIT" || die "stop $IRQ_BALANCE_UNIT 失败（IRQ 仍会被搬回 A72；不切换）" 3
  else
    log "$IRQ_BALANCE_UNIT 已 inactive（无需 stop；before_active=$RB_BEFORE_ACTIVE）"
  fi
else
  warn "$IRQ_BALANCE_UNIT 未安装：跳过 disable/stop（无自动均衡；仍由 oneshot 钉核）"
fi
# 立即写 pin 并回读（在 retire 之前失败 = 旧 C 运行态未动，属音频零变更）
if ev_cmd "$IRQ_PIN" apply --root "$ROOT" --config "$(p "$KHAEN_ETC/irq-affinity.env")"; then
  log "IRQ 钉核完成（pins: $IRQ_PINS）"
else
  die "IRQ 亲和写入/回读失败（T45 admission 将拒绝；旧 C 运行态未动，音频零变更；证据见 install evidence）" 3
fi
# T45 IRQ 放置前置 + 机器可读证据（不是 plan() 本体；plan accepted 证据见 evidence/board-irq-affinity/）
if out="$(py "$DEPLOY_ROOT/lib/irq_affinity.py" check --root "$ROOT" --pins "$IRQ_PINS" --t45-map "$IRQ_T45_MAP" --dsp-cpus "$IRQ_DSP_CPUS" --json "$EVIDENCE_DIR/irq-affinity.json" 2>&1)"; then
  log "IRQ 亲和校验 PASS：$(printf '%s' "$out" | tail -n 1)"
else
  die "IRQ 亲和校验失败（T45 IRQ 放置前置）：$(printf '%s' "$out" | tr '\n' ' ' | cut -c1-400)" 3
fi

# ---- MOTU 48k keeper（D8/用户禁止 192k；静音 1s，不碰 CueMix） ---------------
KEEPER_DIR="$(p /usr/local/lib/khaen)"
if [ -f "$DEPLOY_ROOT/config/motu-48k-keeper" ]; then
  ev_cmd install -d -m 0755 "$KEEPER_DIR" || die "创建 $KEEPER_DIR 失败" 7
  ev_cmd install -m 0755 "$DEPLOY_ROOT/config/motu-48k-keeper" "$KEEPER_DIR/motu-48k-keeper" || die "安装 MOTU 48k keeper 脚本失败" 3
  ev_cmd install -m 0644 "$DEPLOY_ROOT/config/khaen-motu-48k-keeper.service" "$SYSD_DIR/khaen-motu-48k-keeper.service" || die "安装 keeper service 失败" 3
  ev_cmd install -m 0644 "$DEPLOY_ROOT/config/khaen-motu-48k-keeper.timer" "$SYSD_DIR/khaen-motu-48k-keeper.timer" || die "安装 keeper timer 失败" 3
  # keeper v2 的 CueMix 只读客户端（读 kSampleRate；不装则 stageB 退化为 NOT_VERIFIED）
  if [ -f "$DEPLOY_ROOT/tools/cuemix_client.py" ]; then
    ev_cmd install -m 0644 "$DEPLOY_ROOT/tools/cuemix_client.py" "$KEEPER_DIR/cuemix_client.py" || die "安装 cuemix_client.py 失败（keeper stageB 将不可用）" 3
  else
    warn "缺少 deploy/tools/cuemix_client.py；keeper 只能做 ALSA 空闲兜底"
  fi
  # keeper v2 开关文件（WRITE_BACK 默认 0 = 只观测不写 CueMix；写回路径未证）
  if [ -f "$DEPLOY_ROOT/config/motu-rate-guard.env" ]; then
    ev_cmd install -d -m 0755 "$(p /etc/khaen)" || die "创建 /etc/khaen 失败" 7
    if [ -f "$(p /etc/khaen)/motu-rate-guard.env" ]; then
      log "保留既有 /etc/khaen/motu-rate-guard.env（不覆盖运维已调开关）"
    else
      ev_cmd install -m 0644 "$DEPLOY_ROOT/config/motu-rate-guard.env" "$(p /etc/khaen)/motu-rate-guard.env" || die "安装 motu-rate-guard.env 失败" 3
    fi
  else
    warn "缺少 deploy/config/motu-rate-guard.env；keeper 将用内置默认 WRITE_BACK=0"
  fi
  ev_cmd install -d -m 0755 "$(p /etc/udev/rules.d)" || true
  ev_cmd install -m 0644 "$DEPLOY_ROOT/config/95-khaen-motu-48k-keeper.rules" "$(p /etc/udev/rules.d)/95-khaen-motu-48k-keeper.rules" || die "安装 keeper udev rule 失败（设备重枚举后 48k 无保证；F-43 改为硬失败）" 3
  if command -v udevadm >/dev/null 2>&1; then ev_cmd udevadm control --reload-rules || warn "udevadm reload-rules 失败"; fi
else
  warn "缺少 deploy/config/motu-48k-keeper；MOTU 48k 保持不可用"
fi

# ---- 板级韧性（2026-11-21）：watchdog + panic-on-lockup + pstore 归档 + 温度日志 ----
# 背景：2026-11-20 08:59 板子失去响应约 8h47m，无任何可读证据，靠人为断电才恢复。
KEEPER_DIR_DONE=1
ev_cmd install -d -m 0755 "$(p /etc/sysctl.d)" || die "创建 /etc/sysctl.d 失败" 7
ev_cmd install -m 0644 "$DEPLOY_ROOT/config/99-khaen-resilience.conf" "$(p /etc/sysctl.d)/99-khaen-resilience.conf" || die "安装 99-khaen-resilience.conf 失败" 3
if command -v sysctl >/dev/null 2>&1; then
  ev_cmd sysctl --system >/dev/null 2>&1 || warn "sysctl --system 返回非零（部分键可能不支持，见 journal）"
  log "sysctl 生效检查：panic_on_oops=$(cat /proc/sys/kernel/panic_on_oops 2>/dev/null) hardlockup_panic=$(cat /proc/sys/kernel/hardlockup_panic 2>/dev/null)"
fi
ev_cmd install -d -m 0755 "$(p /etc/systemd/system.conf.d)" || die "创建 system.conf.d 失败" 7
ev_cmd install -m 0644 "$DEPLOY_ROOT/config/10-khaen-watchdog.conf" "$(p /etc/systemd/system.conf.d)/10-khaen-watchdog.conf" || die "安装 10-khaen-watchdog.conf 失败" 3
if [ -f "$DEPLOY_ROOT/config/pstore-archive" ]; then
  ev_cmd install -m 0755 "$DEPLOY_ROOT/config/pstore-archive" "$KEEPER_DIR/pstore-archive" || die "安装 pstore-archive 失败" 3
  ev_cmd install -m 0644 "$DEPLOY_ROOT/config/khaen-pstore-archive.service" "$SYSD_DIR/khaen-pstore-archive.service" || die "安装 pstore-archive unit 失败" 3
  ev_cmd "$KHAEN_SYSTEMCTL" enable khaen-pstore-archive.service || warn "enable pstore-archive 失败"
fi
if [ -f "$DEPLOY_ROOT/config/thermal-log" ]; then
  ev_cmd install -m 0755 "$DEPLOY_ROOT/config/thermal-log" "$KEEPER_DIR/thermal-log" || die "安装 thermal-log 失败" 3
  ev_cmd install -m 0644 "$DEPLOY_ROOT/config/khaen-thermal-log.service" "$SYSD_DIR/khaen-thermal-log.service" || die "安装 thermal-log service 失败" 3
  ev_cmd install -m 0644 "$DEPLOY_ROOT/config/khaen-thermal-log.timer" "$SYSD_DIR/khaen-thermal-log.timer" || die "安装 thermal-log timer 失败" 3
  ev_cmd "$KHAEN_SYSTEMCTL" enable khaen-thermal-log.timer || warn "enable thermal-log.timer 失败"
fi
if [ -e /dev/watchdog ]; then
  log "/dev/watchdog 存在（RuntimeWatchdogSec 生效需 daemon-reload；06-verify V11 会核）"
else
  warn "/dev/watchdog 不存在：硬件看门狗不可用（V11.watchdog 将 NOT_VERIFIED）"
fi

# ---- systemd 装载（不 start） ------------------------------------------------
ev_cmd "$KHAEN_SYSTEMCTL" daemon-reload || die "daemon-reload 失败" 3
for u in "${NEW_STACK_UNITS[@]}"; do
  if sunit_exists "$u"; then log "unit 可加载: $u"; else warn "unit 未找到/加载失败: $u"; fi
done
if [ "$NO_ENABLE" = "0" ]; then
  ev_cmd "$KHAEN_SYSTEMCTL" enable khaen-rt-stack.target || die "enable khaen-rt-stack.target 失败" 3
fi
# keeper 开机先于 web/rt-host 置 48k；timer 周期兜底，udev 规则处理重枚举
if [ -f "$SYSD_DIR/khaen-motu-48k-keeper.service" ] && [ -f "$SYSD_DIR/khaen-motu-48k-keeper.timer" ]; then
  ev_cmd "$KHAEN_SYSTEMCTL" enable khaen-motu-48k-keeper.service khaen-motu-48k-keeper.timer || die "enable MOTU 48k keeper 失败（boot 48k 无保证；T28-P2-3 改为硬失败）" 3
else
  warn "MOTU 48k keeper unit 未安装，跳过 enable"
fi

# 记录安装（供 rollback/审计）
cat > "$EVIDENCE_DIR/install-record.json" <<JSON
{
  "schema": "khaen-rt-stack/install-record/v1",
  "release_id": "$REL_ID",
  "release_dir": "$REL_DIR",
  "previous_current": "${PREV_CURRENT:-}",
  "manifest_sha256": "$(sha256_of "$REL_DIR/manifest.json")",
  "installed_at": "$(now_iso)",
  "enabled_target": $( [ "$NO_ENABLE" = "0" ] && printf true || printf false ),
  "started": false,
  "current_mode": "$CURRENT_MODE",
  "installed_configs": [
    "$ETC_DIR/rt-host.env", "$ETC_DIR/rt-daemon.env",
    "$ETC_DIR/rt-host.toml", "$ETC_DIR/rt-daemon.toml",
    "$ETC_DIR/irq-affinity.env"
  ],
  "irq_persistence": {
    "enabled": true,
    "pins": "$IRQ_PINS",
    "t45_map": "$IRQ_T45_MAP",
    "dsp_cpus": "$IRQ_DSP_CPUS",
    "pin_script": "$IRQ_PIN",
    "pin_script_sha256": "$(sha256_of "$IRQ_PIN")",
    "oneshot_unit": "khaen-irq-affinity.service",
    "irqbalance": {
      "unit": "$IRQ_BALANCE_UNIT",
      "before_enabled": "$RB_BEFORE_ENABLED",
      "before_active": "$RB_BEFORE_ACTIVE",
      "after": "disabled+inactive"
    },
    "evidence": "irq-affinity.json"
  },
  "boot_race_fix_b1": {
    "name": "B1-boot-irq-wait",
    "unit_execstartpre_wait": true,
    "wait_env": "KHAEN_IRQ_WAIT_SECS",
    "legacy_live_only_dropin": "$LEGACY_STATE",
    "evidence": "migrated-dropins/"
  }
}
JSON
log "安装完成：$REL_DIR（current 已切换；unit 已装载/enable，未 start）"
exit 0
