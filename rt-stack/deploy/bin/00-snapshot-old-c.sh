#!/usr/bin/env bash
# T28 00-snapshot-old-c.sh —— 旧 C/旧音频栈**只读**快照（退役前基线；S0 的 C 相关子集 + T28 清单）
#
# 不变量：本脚本不 stop/start/enable/disable 任何服务、不打开 PCM、不写设备、不改配置；
#         只读 /proc、/sys、systemd 查询、sha256/stat、有界 journal/ss 查询。
# 输出：证据目录（默认 /var/lib/khaen-rt-stack/evidence/<ts>-00-snapshot-old-c/）：
#   meta.tsv files.tsv units.tsv procs.tsv runtime.tsv raw/ commands.log snapshot.json MANIFEST.sha256
# 退出码：0 成功；2 用法/缺命令；3 只读自检失败（快照期间状态变化）；7 环境不支持。
#
# 用法：
#   sudo ./00-snapshot-old-c.sh --live --out /var/lib/khaen-rt-stack/evidence   # 真正采集
#   ./00-snapshot-old-c.sh                 # 默认 dry-run：只打印将执行的只读命令
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

OUT_BASE=""
LABEL="pre-retire"
usage() { sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//' >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT_BASE="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

[ "$DRY_RUN" = "0" ] && { require_root_live; }
if [ -n "$OUT_BASE" ]; then
  EVIDENCE_DIR="${OUT_BASE%/}/${RUN_TS}-00-snapshot-old-c"
else
  EVIDENCE_DIR="${KHAEN_EVIDENCE_RUN:-$(p "$KHAEN_EVIDENCE_BASE")/${RUN_TS}-00-snapshot-old-c}"
fi
RAW="$EVIDENCE_DIR/raw"
CMDLOG="$EVIDENCE_DIR/commands.log"
META="$EVIDENCE_DIR/meta.tsv"
FILES="$EVIDENCE_DIR/files.tsv"
UNITS="$EVIDENCE_DIR/units.tsv"
PROCS="$EVIDENCE_DIR/procs.tsv"
RUNTIME="$EVIDENCE_DIR/runtime.tsv"

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 快照将写入（不会实际创建）: $EVIDENCE_DIR"
else
  mkdir -p "$RAW" "$EVIDENCE_DIR" || die "无法创建证据目录: $EVIDENCE_DIR" 7
fi

# ---------------------------------------------------------------------------
# 记录器
# ---------------------------------------------------------------------------
meta() { [ "$DRY_RUN" = "1" ] && { log "[dry-run] meta $1=$2"; return 0; }; printf '%s\t%s\n' "$1" "$2" >> "$META"; }
file_row() {  # kind path
  local kind="$1" path="$2" fp status sha size
  fp="$(p "$path")"
  if [ -e "$fp" ]; then
    status="OK"; sha="$(sha256_of "$fp")"; size="$(file_size_of "$fp")"
    if [ -L "$fp" ]; then status="SYMLINK"; fi
  else
    status="MISSING"; sha=""; size="0"
  fi
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] file[$kind] $path ($status)"; return 0; fi
  printf '%s\t%s\t%s\t%s\t%s\n' "$kind" "$path" "$size" "$sha" "$status" >> "$FILES"
}
raw_cap() {  # name cmd...
  local name="$1"; shift
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run][ro] $* > raw/$name.txt"; return 0; fi
  { printf '# cmd: %s\n' "$*"; "$@"; } > "$RAW/$name.txt" 2>&1
  printf '%s\texit=%s\t%s\n' "$(ts)" "$?" "$*" >> "$CMDLOG"
}
raw_cap_sh() {  # name shell-string
  local name="$1" cmd="$2"
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run][ro] $cmd > raw/$name.txt"; return 0; fi
  { printf '# cmd: %s\n' "$cmd"; bash -c "$cmd"; } > "$RAW/$name.txt" 2>&1
  printf '%s\texit=%s\t%s\n' "$(ts)" "$?" "$cmd" >> "$CMDLOG"
}
unit_row() {
  local u="$1" a e note=""
  a="$("$KHAEN_SYSTEMCTL" is-active "$u" 2>/dev/null || true)"; [ -n "$a" ] || a="unknown"
  e="$(sunit_enabled_state "$u")"; [ -n "$e" ] || e="unknown"
  if ! sunit_exists "$u"; then note="NOT_PRESENT"; fi
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] unit $u active=$a enabled=$e $note"; return 0; fi
  printf '%s\t%s\t%s\t%s\n' "$u" "$a" "$e" "$note" >> "$UNITS"
}
proc_row() {  # pattern
  local pat="$1" line
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] pgrep -a -f $pat"; return 0; fi
  pgrep -a -f "$pat" 2>/dev/null | while IFS= read -r line; do
    printf '%s\t%s\n' "$pat" "$line" >> "$PROCS"
  done
}
runtime_row() {
  local path="$1" fp status sha size
  fp="$(p "$path")"
  if [ -e "$fp" ]; then status="OK"; sha="$(sha256_of "$fp")"; size="$(file_size_of "$fp")"; else status="MISSING"; sha=""; size="0"; fi
  if [ "$DRY_RUN" = "1" ]; then log "[dry-run] runtime $path ($status)"; return 0; fi
  printf '%s\t%s\t%s\t%s\n' "$path" "$status" "$size" "$sha" >> "$RUNTIME"
}

# 完整性指纹：相关 unit active 集合 + /proc/asound/devices + /dev/snd 列表
integrity_digest() {
  local units_digest asound_digest snd_digest
  units_digest="$(for u in "${ALL_OLD_AUDIO_UNITS[@]}" "${NEW_STACK_UNITS[@]}"; do printf '%s=%s\n' "$u" "$("$KHAEN_SYSTEMCTL" is-active "$u" 2>/dev/null || true)"; done | sha256sum | awk '{print $1}')"
  asound_digest="$(cat "$(p /proc/asound/devices)" 2>/dev/null | sha256sum | awk '{print $1}')"
  snd_digest="$(ls -la "$(p /dev/snd)" 2>/dev/null | sha256sum | awk '{print $1}')"
  printf '%s' "$units_digest$asound_digest$snd_digest" | sha256sum | awk '{print $1}'
}

log "== 00 snapshot-old-c label=$LABEL root=$ROOT evidence=$EVIDENCE_DIR"
meta generated_at "$(ts)"
meta label "$LABEL"
meta root "$ROOT"
meta mode "READ_ONLY"

# ---------------------------------------------------------------------------
# 1. 身份/内核/RT 环境
# ---------------------------------------------------------------------------
raw_cap identity sh -c 'hostname; uname -a; cat /etc/debian_version 2>/dev/null; date -u +%Y-%m-%dT%H:%M:%SZ'
raw_cap cmdline cat /proc/cmdline
raw_cap realtime cat /sys/kernel/realtime
raw_cap cpu_online cat /sys/devices/system/cpu/online
raw_cap lscpu lscpu -e=CPU,CORE,SOCKET,ONLINE,MAXMHZ,MINMHZ
raw_cap rt_runtime cat /proc/sys/kernel/sched_rt_runtime_us
raw_cap limits_self sh -c 'id; ulimit -r; ulimit -l'
raw_cap date sh -c 'date; timedatectl show -p NTPSynchronized 2>/dev/null || true'
raw_cap df sh -c 'df -h / /opt /var/lib 2>/dev/null || true'

BEFORE_DIGEST="$(integrity_digest)"
meta integrity_before "$BEFORE_DIGEST"

# ---------------------------------------------------------------------------
# 2. unit 清单与状态（旧栈 9 + web + 新栈 + 可能的 socket/timer/drop-in）
# ---------------------------------------------------------------------------
raw_cap_sh unit_files "systemctl list-unit-files 'khaen-*' --no-pager 2>&1 || true"
raw_cap_sh unit_list "systemctl list-units 'khaen-*' --all --no-pager 2>&1 || true"
raw_cap_sh unit_cat "systemctl cat khaen-audio.target khaen-camilladsp.service khaen-statime-inferno.service khaen-aes67-daemon.service khaen-ravenna-lkm.service khaen-ptp4l-master.service khaen-phc2sys.service khaen-boot-mode.service khaen-upmix-autodetect.service khaen-audio-web.service 2>&1 || true"
raw_cap_sh wants "ls -la /etc/systemd/system/*.wants/ 2>/dev/null || true"
for u in "${ALL_OLD_AUDIO_UNITS[@]}" "${NEW_STACK_UNITS[@]}"; do unit_row "$u"; done
# socket/timer/drop-in 防 dependency 自动激活（bounded）
raw_cap_sh sockets "systemctl list-unit-files 'khaen-*.socket' 'khaen-*.timer' --no-pager 2>&1 || true"
raw_cap_sh dropins "find /etc/systemd/system -maxdepth 2 -path '*khaen-*.service.d/*.conf' -print 2>/dev/null | sort || true"

# ---------------------------------------------------------------------------
# 3. 进程快照（旧 C / 旧 web / 保留 AoIP 服务）
# ---------------------------------------------------------------------------
raw_cap_sh ps_rt "ps -eLo pid,tid,cls,rtprio,pri,psr,comm --no-headers 2>/dev/null | sort -n || true"
for pat in "${C_PROCESS_PATTERNS[@]}" khaen-audio-web statime aes67-daemon; do proc_row "$pat"; done
raw_cap_sh proc_holders "find /proc/[0-9]*/fd -maxdepth 1 -type l 2>/dev/null | head -n 4000 | xargs -r ls -l 2>/dev/null | grep -E ' /dev/snd/' || true"

# ---------------------------------------------------------------------------
# 4. 二进制/库/模块 sha256（板端实际版本；S0-10 子集）
# ---------------------------------------------------------------------------
OLD_BINARIES=(
  /usr/local/bin/camilladsp
  /usr/local/bin/khaen-audio-web
  /usr/local/bin/khaen-mode
  /usr/local/bin/khaen-channelmap
  /usr/local/bin/khaen-upmix
  /usr/local/bin/khaen-ptp-status
  /usr/local/bin/statime
  /usr/local/bin/inferno2pipe
  /usr/local/bin/aes67-daemon
  /usr/sbin/ptp4l
  /usr/sbin/phc2sys
  /usr/sbin/pmc
  /usr/local/lib/khaen/lib.sh
  /usr/local/lib/khaen/ptp-status-hook
  /usr/local/lib/khaen/ucm-backport-mk5.sh
  /usr/lib/aarch64-linux-gnu/alsa-lib/libasound_module_pcm_inferno.so
)
for b in "${OLD_BINARIES[@]}"; do file_row binary "$b"; done
KVER="$(uname -r 2>/dev/null || true)"
[ -n "$KVER" ] && file_row library "/lib/modules/$KVER/extra/MergingRavennaALSA.ko"
for row in /usr/sbin/ptp4l /usr/sbin/phc2sys /usr/sbin/pmc; do file_row binary "$row"; done
raw_cap_sh symlinks "for p in /usr/local/bin/camilladsp /usr/local/bin/khaen-audio-web /usr/local/bin/khaen-mode /usr/local/bin/khaen-upmix; do readlink -f \"\$p\" 2>/dev/null; done"

# ---------------------------------------------------------------------------
# 5. 配置/unit/资产生成 sha256（旧栈 config；web auth 只 hash 不读内容）
# ---------------------------------------------------------------------------
OLD_CONFIGS=(
  /etc/khaen-audio/layout-presets.yml
  /etc/khaen-audio/channelmap.yml
  /etc/khaen-audio/upmix/presets.yml
  /etc/khaen-audio/input-mode.json
  /etc/camilladsp/dante.yml
  /etc/camilladsp/aes67.yml
  /etc/camilladsp/coeffs/README.md
  /etc/default/khaen-audio
  /etc/asound.conf
  /etc/modprobe.d/khaen-audio.conf
  /etc/ptp4l-master-aes67.conf
  /etc/statime/inferno-ptpv1.toml
  /etc/aes67-daemon/daemon.conf
  /etc/security/limits.d/95-khaen-audio.conf
  /etc/udev/rules.d/95-motu-ultralite-mk5.rules
  /etc/tmpfiles.d/khaen-audio.conf
  /etc/systemd/system/khaen-audio.target
  /etc/systemd/system/khaen-camilladsp.service
  /etc/systemd/system/khaen-upmix-autodetect.service
  /etc/systemd/system/khaen-audio-web.service
  /etc/khaen-audio-web/config.toml
  /etc/khaen-audio-web/auth.json
)
for c in "${OLD_CONFIGS[@]}"; do file_row config "$c"; done
raw_cap_sh tree_etc "find /etc/khaen-audio /etc/camilladsp /etc/khaen-audio-web -maxdepth 3 -type f -printf '%p %s\n' 2>/dev/null | sort || true"
raw_cap_sh tree_varlib "ls -la /var/lib/khaen/ 2>/dev/null; find /var/lib/khaen -maxdepth 3 -type f -printf '%p %s\n' 2>/dev/null | sort | head -n 300 || true"
raw_cap_sh pkg_versions "dpkg -l 2>/dev/null | grep -Ei 'alsa|boost|avahi|linuxptp|jq|statime' | head -n 120 || true"
raw_cap_sh sha_release "sha256sum /etc/khaen-audio-manifest.txt 2>/dev/null || true"

# ---------------------------------------------------------------------------
# 6. 运行态（/run/khaen + /etc/khaen/last-mode + 新栈 last-good）
# ---------------------------------------------------------------------------
for f in "${C_ENTRY_FILES[@]}" /run/khaen/state.json /run/khaen/config.json /run/khaen/input-content.json /run/khaen/ptp-lock /var/lib/khaen-rt-stack/last-good.json; do
  runtime_row "$f"
done
raw_cap_sh run_khaen "ls -la /run/khaen/ 2>/dev/null; for f in /run/khaen/mode /etc/khaen/last-mode; do echo \"== \$f\"; cat \"\$f\" 2>/dev/null || echo MISSING; done"
raw_cap_sh sockets_lsten "ss -ltnp 2>/dev/null | grep -E ':(80|1234|8080|8090)\\b' || true"

# ---------------------------------------------------------------------------
# 7. ALSA/设备（只读 /proc；绝不打开 PCM）
# ---------------------------------------------------------------------------
raw_cap cards cat /proc/asound/cards
raw_cap modules cat /proc/asound/modules
raw_cap devices cat /proc/asound/devices
raw_cap_sh stream0 'for f in /proc/asound/card*/stream0; do echo "== $f"; cat "$f"; done 2>/dev/null || true'
raw_cap_sh pcm_status 'for f in /proc/asound/card*/pcm*/sub*/status; do [ -r "$f" ] && echo "$f: $(cat "$f")"; done 2>/dev/null || true'
raw_cap_sh dev_snd 'ls -l /dev/snd 2>/dev/null || true'
raw_cap_sh lsmod 'lsmod 2>/dev/null | grep -E "Merging|snd" || true'
raw_cap_sh net 'ip -br addr 2>/dev/null || true; ls -l /dev/ptp* 2>/dev/null || true'

# ---------------------------------------------------------------------------
# 8. 有界 journal（只读；不清理/不改级别）
# ---------------------------------------------------------------------------
raw_cap_sh journal_c 'journalctl -u khaen-camilladsp --since "-24h" --no-pager -q 2>/dev/null | tail -n 80 || true'
raw_cap_sh journal_web 'journalctl -u khaen-audio-web --since "-24h" --no-pager -q 2>/dev/null | tail -n 40 || true'

# ---------------------------------------------------------------------------
# 9. 只读自检 + 组装
# ---------------------------------------------------------------------------
AFTER_DIGEST="$(integrity_digest)"
meta integrity_after "$AFTER_DIGEST"
if [ "$DRY_RUN" = "0" ]; then
  if [ "$BEFORE_DIGEST" = "$AFTER_DIGEST" ]; then meta integrity_equal true; else meta integrity_equal false; warn "快照期间状态变化：before=$BEFORE_DIGEST after=$AFTER_DIGEST"; fi
else
  log "[dry-run] 跳过 integrity 比较"
fi

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 将运行: python3 lib/snapshot_to_json.py $EVIDENCE_DIR"
  log "[dry-run] 将运行: sha256sum raw/* snapshot.json > MANIFEST.sha256"
  exit 0
fi

py "$(cd "$HERE/../lib" && pwd)/snapshot_to_json.py" "$EVIDENCE_DIR" || exit 3
( cd "$EVIDENCE_DIR" && sha256sum raw/* snapshot.json 2>/dev/null | sort > MANIFEST.sha256 )
log "快照完成: $EVIDENCE_DIR/snapshot.json"
exit 0
