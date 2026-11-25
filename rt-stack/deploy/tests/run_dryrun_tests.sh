#!/usr/bin/env bash
# T28 真实干跑测试套件（Git Bash / Linux 均可；全程 mock，不连板、不触碰真实 systemd/设备）
#
# 覆盖：语法、旧 C 快照、授权窗口门、preflight、bundle 打包、安装、退役、设备独占、验证、
#       last-good 记录、回退（last-good/mute）、原子切换成功/启动失败回退、rollback 契约、unit 语法、ELF 架构。
# 全部命令真实执行（bash/python），systemctl/pgrep/pkill/fuser/... 由 tests/mocks 拦截。
# 用法: ./run_dryrun_tests.sh [--out DIR] [--keep] [--only CASE]
# 退出码: 0 全部 PASS；3 有 FAIL。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY="$(cd "$HERE/.." && pwd)"
RS="$(cd "$DEPLOY/.." && pwd)"
PROJECT="$(cd "$RS/.." && pwd)"
MOCKS="$HERE/mocks"
OUT=""; KEEP=0; ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --only) ONLY="$2"; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUT" ] || OUT="$RS/evidence/migration-plan/dryrun"
mkdir -p "$OUT"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/tspi-t28-tests-XXXXXX")" || { echo "mktemp failed" >&2; exit 7; }
cleanup() { [ "$KEEP" = "1" ] || rm -rf "$TMP"; }
trap cleanup EXIT
winpath() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
BOARD="$TMP/board"
ROOT="$(winpath "$BOARD")"
export PATH="$MOCKS:$PATH"
export TSPI_PYTHON=python TSPI_POLL_SECS=0 TSPI_DEPLOY_ROOT="$ROOT"
export MOCK_STATE="$TMP/mock" MOCK_LOG="$TMP/mock/log" MOCK_PROCS="$TMP/mock/procs"
export MOCK_SS="$TMP/mock/ss.txt" MOCK_SND_BUSY="$TMP/mock/snd_busy" MOCK_HEALTH_CODE=200 MOCK_HEALTH_EXIT=0
export MOCK_FAIL_MATCH=""
LOGD="$TMP/logs"; mkdir -p "$LOGD" "$MOCK_STATE" "$BOARD"
RESULTS="$TMP/results.log"; : > "$RESULTS"
PASS=0; FAIL=0; CASE_ERRS=0
. "$DEPLOY/lib/common.sh"
WIN="$TMP/win.json"
PYBIN="python"

fail_msg() { CASE_ERRS=$((CASE_ERRS + 1)); echo "  ASSERT-FAIL: $*"; }
a_rc() { local want="$1" got="$2" msg="$3"; [ "$got" = "$want" ] || fail_msg "$msg (want rc=$want got=$got)"; }
a_file() { [ -f "$1" ] || fail_msg "缺文件: $1"; }
a_absent() { [ -e "$1" ] && fail_msg "不应存在: $1"; return 0; }
a_dir() { [ -d "$1" ] || fail_msg "缺目录: $1"; }
a_contains() { grep -q -- "$2" "$1" 2>/dev/null || fail_msg "文件 $1 不含: $2"; }
a_not_contains() { grep -q -- "$2" "$1" 2>/dev/null && fail_msg "文件 $1 不应含: $2"; return 0; }
a_eq() { [ "$1" = "$2" ] || fail_msg "$3 (want=$1 got=$2)"; }
a_json() { python - "$1" "$2" <<'PYEOF' || fail_msg "JSON 断言失败: $1 :: $2"
import json,sys
d=json.load(open(sys.argv[1],encoding="utf-8"))
expr=sys.argv[2]
ok=eval(expr,{"d":d,"len":len})
raise SystemExit(0 if ok else 1)
PYEOF
}
a_log_no_c_start() {
  if grep -Eq 'systemctl (start|restart|try-restart|reload-or-restart|enable|unmask) [^ ]*(camilladsp|tspi-upmix-autodetect)' "$MOCK_LOG" 2>/dev/null; then
    fail_msg "出现启动/使能旧 C 的命令: $(grep -E 'systemctl (start|restart|enable|unmask) [^ ]*(camilladsp|tspi-upmix-autodetect)' "$MOCK_LOG" | head -n 2 | tr '\n' ' ')"
  fi
  return 0
}

reset_board() {
  rm -rf "$BOARD" "$MOCK_STATE"
  mkdir -p "$BOARD/etc/systemd/system" "$BOARD/etc/tspi" "$BOARD/etc/tspi-rt-stack" \
           "$BOARD/opt/tspi-rt-stack/releases" "$BOARD/var/lib/tspi-rt-stack/evidence" \
           "$BOARD/run/tspi" "$BOARD/dev/snd" "$BOARD/proc/asound/card4/pcm0p/sub0" \
           "$BOARD/proc/4321/fd" "$BOARD/proc/4322/fd" \
           "$BOARD/proc/irq/56" "$BOARD/proc/irq/57" "$BOARD/proc/irq/58" \
           "$BOARD/sys/devices/system/cpu"
  printf ' 4 [UltraLitemk5   ]: USB-Audio - UltraLite mk5 at usb-0000:00:14.0-1, high speed\n' > "$BOARD/proc/asound/cards"
  # task-192 夹具：T45 依赖 IRQ 初始在 A72（58→7、56→6、57→4；task-189 before 实测值）
  # 夹具不造 effective_affinity_list：真实内核会在写 smp_affinity_list 后同步它；无该文件时
  # irq-affinity-pin / lib/irq_affinity.py 回落读 smp_affinity_list（板端真实路径见 NOT_RUN 说明）。
  for pair in 58:7 56:6 57:4; do
    printf '%s\n' "${pair##*:}" > "$BOARD/proc/irq/${pair%%:*}/smp_affinity_list"
  done
  printf '0-7\n' > "$BOARD/sys/devices/system/cpu/online"
  : > "$BOARD/sys/devices/system/cpu/isolated"
  printf 'closed\n' > "$BOARD/proc/asound/card4/pcm0p/sub0/status"
  printf 'Max realtime priority            99\nMax locked memory              unlimited\n' > "$BOARD/proc/4321/limits"
  printf 'Max realtime priority            49\nMax locked memory              67108864\n' > "$BOARD/proc/4322/limits"
  printf 'x\n' > "$BOARD/run/tspi/camilladsp.yml"
  printf 'TSPI_X=1\n' > "$BOARD/run/tspi/camilladsp.env"
  printf 'off\n' > "$BOARD/run/tspi/mode"
  printf 'off\n' > "$BOARD/etc/tspi/last-mode"
  mkdir -p "$MOCK_STATE/units" "$MOCK_STATE/active" "$MOCK_STATE/enabled" "$MOCK_STATE/envs"
  # task-192 夹具：irqbalance 初始 enabled+active（04 安装时应 disable+stop；preflight/06 会拒绝复活）
  printf '[Unit]\nDescription=irqbalance\n' > "$MOCK_STATE/units/irqbalance.service"
  printf 'enabled\n' > "$MOCK_STATE/enabled/irqbalance.service"
  touch "$MOCK_STATE/active/irqbalance.service"
  : > "$MOCK_LOG"; : > "$MOCK_PROCS"; : > "$MOCK_SS"; : > "$MOCK_SND_BUSY"
  export MOCK_FAIL_MATCH=""
  # F5（2026-11-18）：mock 默认按真实 systemd 语义输出 Environment（不含 EnvironmentFile 内容），
  # 防止旧实现的 systemctl-show-Environment 路径在夹具里「假通过」。
  export MOCK_SHOW_ENV_REAL=1
  local u
  for u in tspi-audio.target tspi-camilladsp.service tspi-upmix-autodetect.service tspi-boot-mode.service \
           khaen-audio-web.service tspi-statime-inferno.service tspi-aes67-daemon.service \
           tspi-ravenna-lkm.service tspi-ptp4l-master.service tspi-phc2sys.service; do
    printf 'enabled\n' > "$MOCK_STATE/enabled/$u"
  done
  touch "$MOCK_STATE/active/khaen-audio-web.service"
  printf '100 khaen-audio-web --config /etc/khaen-audio-web/config.toml\n' >> "$MOCK_PROCS"
  cp "$PROJECT"/audio/overlay/etc/systemd/system/*.service "$MOCK_STATE/units/" 2>/dev/null || true
  cp "$PROJECT"/audio/overlay/etc/systemd/system/*.target "$MOCK_STATE/units/" 2>/dev/null || true
  cp "$PROJECT/web/deploy/khaen-audio-web.service" "$MOCK_STATE/units/" 2>/dev/null || true
  cp "$RS"/service/*.service "$RS"/service/*.target "$MOCK_STATE/units/" 2>/dev/null || true
}
make_window() {
  python - "$(winpath "$1")" "${2:-valid}" <<'PYEOF'
import json,sys,datetime
out,mode=sys.argv[1],sys.argv[2]
now=datetime.datetime.now().astimezone()
if mode=="valid":
    s=now-datetime.timedelta(minutes=5); e=now+datetime.timedelta(minutes=60)
elif mode=="expired":
    s=now-datetime.timedelta(minutes=120); e=now-datetime.timedelta(minutes=60)
else:
    raise SystemExit("bad mode")
d={"schema":"tspi-rt-stack/switch-window/v1","window_id":"test-"+mode,"authorized_by":"dryrun-test",
   "board_host":"tspi-audio-dsp","start":s.isoformat(timespec="seconds"),"end":e.isoformat(timespec="seconds"),
   "max_duration_min":90,"scope":["snapshot","preflight","retire-c","install-release","start-new","verify","mark-last-good","rollback"],
   "audio_off_required":True,"s1_stop_acknowledged":True,"soak_absent":True,"physical_access":"test-fixture",
   "notes":"dry-run fixture, NOT an authorization"}
open(out,"w",encoding="utf-8").write(json.dumps(d,ensure_ascii=False,indent=1))
PYEOF
}
make_elf_aarch64() {
  python - "$(winpath "$1")" <<'PYEOF'
import struct,sys
h=bytearray(256)
h[0:4]=b"\x7fELF"; h[4]=2; h[5]=1; h[6]=1
struct.pack_into("<H",h,16,2); struct.pack_into("<H",h,18,183); struct.pack_into("<I",h,20,1)
struct.pack_into("<Q",h,24,0); struct.pack_into("<Q",h,32,64); struct.pack_into("<Q",h,40,0)
struct.pack_into("<H",h,52,64); struct.pack_into("<H",h,54,56); struct.pack_into("<H",h,56,0)
open(sys.argv[1],"wb").write(bytes(h))
PYEOF
  chmod +x "$1" 2>/dev/null || true
}
make_pe_x86_64() {
  python - "$(winpath "$1")" <<'PYEOF'
import struct,sys
d=bytearray(256); d[0:2]=b"MZ"; struct.pack_into("<I",d,0x3C,64); d[64:68]=b"PE\x00\x00"; struct.pack_into("<H",d,68,0x8664)
open(sys.argv[1],"wb").write(bytes(d))
PYEOF
}
make_bundle() {  # id -> sets BUNDLE
  local id="$1"
  mkdir -p "$TMP/bin-$id"
  make_elf_aarch64 "$TMP/bin-$id/khaen-rt-host"
  rm -rf "$TMP/bundle-$id"
  bash "$DEPLOY/bin/make-release-bundle.sh" --live --bin-dir "$(winpath "$TMP/bin-$id")" \
    --out "$(winpath "$TMP/bundle-$id")" --release-id "$id" >"$LOGD/bundle-$id.log" 2>&1
  local rc=$?
  BUNDLE="$TMP/bundle-$id"
  return $rc
}
do_switch() {  # id evidence -> rc
  local id="$1" ev="$2"
  bash "$DEPLOY/bin/05-atomic-switch.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$BUNDLE" \
    --allow-first-switch --evidence "$(winpath "$ev")" >"$LOGD/switch-$id.log" 2>&1
}
run_case() {
  local name="$1"
  CASE_ERRS=0
  "case_$name" >"$LOGD/$name.log" 2>&1
  local rc=$?
  if [ "$CASE_ERRS" -eq 0 ] && [ "$rc" -eq 0 ]; then
    PASS=$((PASS + 1)); printf 'PASS %s\n' "$name" | tee -a "$RESULTS"
  else
    FAIL=$((FAIL + 1)); printf 'FAIL %s (assert=%s case_rc=%s)\n' "$name" "$CASE_ERRS" "$rc" | tee -a "$RESULTS"
    tail -n 25 "$LOGD/$name.log" | sed 's/^/    /'
  fi
}

# ============================== cases ==============================
case_help() {
  local f
  for f in "$DEPLOY"/bin/*.sh; do
    bash "$f" --help >/dev/null 2>&1; a_rc 0 $? "help $(basename "$f")"
  done
}
case_syntax() {
  bash "$HERE/check_syntax.sh" --quiet >/dev/null 2>&1; a_rc 0 $? "check_syntax.sh"
  # T28-P1-a 回归：ev_cmd 必须返回真实退出码并如实记 exit=1（旧实现 ! "$@" 吞错恒返回 0）。
  mkdir -p "$TMP/ev-evcmd"
  TSPI_DEPLOY_DRY_RUN=0 TSPI_EVIDENCE_RUN="$TMP/ev-evcmd" \
    bash -c '. '"$(winpath "$DEPLOY")"'/lib/common.sh; ev_cmd false' >/dev/null 2>&1
  a_rc 1 $? "ev_cmd 应传播失败 rc"
  a_contains "$TMP/ev-evcmd/commands.log" "exit=1"
  TSPI_DEPLOY_DRY_RUN=0 TSPI_EVIDENCE_RUN="$TMP/ev-evcmd" \
    bash -c '. '"$(winpath "$DEPLOY")"'/lib/common.sh; ev_cmd true' >/dev/null 2>&1
  a_rc 0 $? "ev_cmd true 应 rc=0"
}
case_snapshot_dryrun() {
  reset_board
  local ev="$TMP/ev-snap-dry"
  bash "$DEPLOY/bin/00-snapshot-old-c.sh" --root "$ROOT" --out "$(winpath "$ev")" --dry-run >"$LOGD/snap-dry.log" 2>&1
  a_rc 0 $? "snapshot --dry-run"
  a_absent "$ev/snapshot.json"
}
case_snapshot_live() {
  reset_board
  local ev="$TMP/ev-snap"
  bash "$DEPLOY/bin/00-snapshot-old-c.sh" --root "$ROOT" --live --out "$(winpath "$ev")" >"$LOGD/snap-live.log" 2>&1
  a_rc 0 $? "snapshot --live"
  local dir; dir="$(ls -d "$ev"/*-00-snapshot-old-c 2>/dev/null | tail -n 1)"
  a_dir "$dir"
  a_file "$dir/snapshot.json"; a_file "$dir/MANIFEST.sha256"; a_file "$dir/files.tsv"; a_file "$dir/units.tsv"
  a_json "$dir/snapshot.json" "d['schema']=='tspi-rt-stack/old-c-snapshot/v1' and len(d['files'])>8 and len(d['units'])>8 and d['integrity_equal'] in ('true','UNKNOWN')"
  a_contains "$dir/files.tsv" "/usr/local/bin/camilladsp"
  a_contains "$dir/snapshot.json" "old-c-snapshot"
}
case_window_absent() {
  reset_board
  bash "$DEPLOY/bin/03-retire-c.sh" --live --root "$ROOT" >"$LOGD/noauth.log" 2>&1
  a_rc 4 $? "retire-c 无授权应 exit 4"
}
case_window_expired() {
  reset_board; make_window "$WIN" expired
  bash "$DEPLOY/bin/03-retire-c.sh" --live --root "$ROOT" --authorize "$WIN" >"$LOGD/expired.log" 2>&1
  a_rc 4 $? "过期窗口应 exit 4"
  a_contains "$LOGD/expired.log" "\"ok\": false"
}
case_preflight_no_release() {
  reset_board; make_window "$WIN" valid
  bash "$DEPLOY/bin/01-preflight-switch.sh" --live --root "$ROOT" --authorize "$WIN" --allow-first-switch >"$LOGD/pf-norel.log" 2>&1
  a_rc 3 $? "无 release 时 preflight 应 exit 3"
  a_contains "$LOGD/pf-norel.log" "current release"
}
case_bundle_build() {
  reset_board
  make_bundle t-001; a_rc 0 $? "make-release-bundle"
  a_file "$BUNDLE/manifest.json"; a_file "$BUNDLE/bin/khaen-rt-host"; a_file "$BUNDLE/SHA256SUMS"; a_file "$BUNDLE/README.deploy.md"
  a_file "$BUNDLE/units/tspi-irq-affinity.service"   # task-192：IRQ 持久化 oneshot 必须随 bundle
  # B1（2026-11-20 板窗）：bundle unit 必须携带开机等待前置（打包门；旧 service/ 不得进 v12）
  a_contains "$BUNDLE/units/tspi-irq-affinity.service" "ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait"
  a_json "$BUNDLE/manifest.json" "d['schema']=='tspi-rt-stack/release/v1' and len(d['files'])>=8 and any(f['kind']=='binary' for f in d['files'])"
  python "$DEPLOY/lib/release_manifest.py" verify "$(winpath "$BUNDLE")" --require-arch aarch64 >/dev/null 2>&1
  a_rc 0 $? "release_manifest verify --require-arch aarch64"
}
case_install_release() {
  reset_board; make_window "$WIN" valid; make_bundle t-001; a_rc 0 $? "bundle"
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$BUNDLE" --evidence "$(winpath "$TMP/ev-install")" >"$LOGD/install.log" 2>&1
  a_rc 0 $? "install-release"
  a_file "$BOARD/opt/tspi-rt-stack/releases/t-001/manifest.json"
  a_file "$BOARD/etc/systemd/system/khaen-rt-host.service"
  a_file "$BOARD/etc/systemd/system/tspi-rt-stack.target"
  a_contains "$MOCK_LOG" "enable tspi-rt-stack.target"
  local rel; rel="$(current_release_dir 2>/dev/null || true)"
  case "$rel" in *t-001*) : ;; *) fail_msg "current 未指向 t-001: $rel" ;; esac
  a_file "$BOARD/etc/tspi-rt-stack/rt-host.env"
  # F-18/T28-P0-2：首次安装必须落 toml；已有文件不得覆盖。
  a_file "$BOARD/etc/tspi-rt-stack/rt-host.toml"
  a_file "$BOARD/etc/tspi-rt-stack/rt-daemon.toml"
  printf '# local override (must be preserved)\n[host]\nblock_size = 64\n' > "$BOARD/etc/tspi-rt-stack/rt-host.toml"
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$BUNDLE" --evidence "$(winpath "$TMP/ev-install2")" >>"$LOGD/install2.log" 2>&1
  a_rc 0 $? "二次安装（不覆盖 toml）"
  a_contains "$BOARD/etc/tspi-rt-stack/rt-host.toml" "local override"
  a_contains "$MOCK_LOG" "enable tspi-motu-48k-keeper.service"
  # task-192：04 安装时必须 disable+stop irqbalance、写 IRQ pin 并回读，装 pin 脚本/env/oneshot unit
  a_contains "$MOCK_LOG" "disable irqbalance.service"
  a_contains "$MOCK_LOG" "stop irqbalance.service"
  a_eq "1" "$(cat "$BOARD/proc/irq/58/smp_affinity_list")" "IRQ58 应钉到 CPU1"
  a_eq "2" "$(cat "$BOARD/proc/irq/56/smp_affinity_list")" "IRQ56 应钉到 CPU2"
  a_eq "3" "$(cat "$BOARD/proc/irq/57/smp_affinity_list")" "IRQ57 应钉到 CPU3"
  a_file "$BOARD/usr/local/lib/tspi/irq-affinity-pin"
  a_file "$BOARD/etc/tspi-rt-stack/irq-affinity.env"
  a_file "$BOARD/etc/systemd/system/tspi-irq-affinity.service"
  # B1（2026-11-20 板窗）：装到板上的 oneshot 必须含开机等待前置；pin 脚本支持 wait；record 记录 B1 字段
  a_contains "$BOARD/etc/systemd/system/tspi-irq-affinity.service" "ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait"
  a_contains "$BOARD/usr/local/lib/tspi/irq-affinity-pin" "irq-affinity-pin wait"
  a_json "$TMP/ev-install/install-record.json" "d['boot_race_fix_b1']['unit_execstartpre_wait'] is True and d['boot_race_fix_b1']['legacy_live_only_dropin']=='absent'"
  a_file "$TMP/ev-install/irq-affinity.json"
  a_json "$TMP/ev-install/irq-affinity.json" "d['overall_ok'] is True and d['t45']['plan_equivalent']=='PASS'"
}
case_install_bad_arch() {
  reset_board; make_window "$WIN" valid
  mkdir -p "$TMP/bin-bad"
  make_pe_x86_64 "$TMP/bin-bad/khaen-rt-host"
  bash "$DEPLOY/bin/make-release-bundle.sh" --live --allow-foreign-arch --bin-dir "$(winpath "$TMP/bin-bad")" --out "$(winpath "$TMP/bundle-bad")" --release-id bad >"$LOGD/bundle-bad.log" 2>&1
  a_rc 0 $? "foreign bundle 打包"
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$TMP/bundle-bad" --evidence "$(winpath "$TMP/ev-bad")" >"$LOGD/install-bad.log" 2>&1
  a_rc 3 $? "PE 二进制安装应 exit 3"
  a_absent "$BOARD/opt/tspi-rt-stack/current"
}
case_retire_live() {
  reset_board; make_window "$WIN" valid
  bash "$DEPLOY/bin/03-retire-c.sh" --live --root "$ROOT" --authorize "$WIN" --evidence "$(winpath "$TMP/ev-retire")" >"$LOGD/retire.log" 2>&1
  a_rc 0 $? "retire-c live"
  a_contains "$MOCK_LOG" "disable tspi-camilladsp.service"
  a_contains "$MOCK_LOG" "mask tspi-camilladsp.service"
  a_contains "$MOCK_LOG" "disable tspi-upmix-autodetect.service"
  a_contains "$MOCK_LOG" "stop khaen-audio-web.service"
  a_contains "$MOCK_LOG" "disable tspi-audio.target"
  a_log_no_c_start
  a_absent "$BOARD/run/tspi/camilladsp.yml"
  a_absent "$BOARD/run/tspi/camilladsp.env"
  a_absent "$BOARD/etc/tspi/last-mode"
  a_absent "$BOARD/run/tspi/mode"
  a_file "$TMP/ev-retire/result.txt"
  a_contains "$TMP/ev-retire/result.txt" "FAILS=0"
}
case_verify_detects_c() {
  reset_board
  systemctl start khaen-rt-host.service >/dev/null 2>&1
  printf 'enabled\n' > "$MOCK_STATE/enabled/tspi-camilladsp.service"
  touch "$MOCK_STATE/active/tspi-camilladsp.service"
  printf '999 camilladsp -p 1234 -a 127.0.0.1 -l warn --gain1=-120 --gain2=0 /run/tspi/camilladsp.yml\n' >> "$MOCK_PROCS"
  bash "$DEPLOY/bin/06-verify-switch.sh" --live --root "$ROOT" --evidence "$(winpath "$TMP/ev-verify-bad")" >"$LOGD/verify-bad.log" 2>&1
  a_rc 5 $? "C 仍 active 时 verify 应 exit 5"
  a_contains "$TMP/ev-verify-bad/verify-report.json" "FAIL"
  # F-42：disabled 但未 mask 也算 C 未退役（只查 enabled 不够）。
  rm -f "$MOCK_STATE/active/tspi-camilladsp.service"
  grep -v "camilladsp" "$MOCK_PROCS" > "$MOCK_PROCS.t" 2>/dev/null; mv -f "$MOCK_PROCS.t" "$MOCK_PROCS" 2>/dev/null || true
  printf 'disabled\n' > "$MOCK_STATE/enabled/tspi-camilladsp.service"
  systemctl start khaen-rt-host.service >/dev/null 2>&1
  bash "$DEPLOY/bin/06-verify-switch.sh" --live --root "$ROOT" --evidence "$(winpath "$TMP/ev-verify-nomask")" >"$LOGD/verify-nomask.log" 2>&1
  a_rc 5 $? "C disabled 但未 mask 时 verify 应 exit 5"
  a_contains "$TMP/ev-verify-nomask/verify-report.json" "not-masked"
}
case_rollback_mute() {
  reset_board; make_window "$WIN" valid
  local u
  for u in tspi-camilladsp.service tspi-upmix-autodetect.service tspi-boot-mode.service khaen-audio-web.service; do
    printf 'masked\n' > "$MOCK_STATE/enabled/$u"
  done
  printf 'disabled\n' > "$MOCK_STATE/enabled/tspi-audio.target"
  systemctl stop khaen-audio-web.service >/dev/null 2>&1
  systemctl start khaen-rt-host.service >/dev/null 2>&1
  bash "$DEPLOY/bin/08-rollback.sh" --live --root "$ROOT" --authorize "$WIN" --evidence "$(winpath "$TMP/ev-rb-mute")" --mute >"$LOGD/rb-mute.log" 2>&1
  a_rc 10 $? "无 last-good 的 --mute 应 exit 10"
  a_file "$BOARD/etc/systemd/system/khaen-rt-host.service.d/90-safe-mute.conf"
  a_file "$BOARD/etc/tspi-rt-stack/90-safe-mute.env"
  # F-17：drop-in 用 EnvironmentFile（不是 Environment=），env 写正确变量名，能压过 rt-host.env 的 normal。
  a_contains "$BOARD/etc/systemd/system/khaen-rt-host.service.d/90-safe-mute.conf" "EnvironmentFile=/etc/tspi-rt-stack/90-safe-mute.env"
  a_not_contains "$BOARD/etc/systemd/system/khaen-rt-host.service.d/90-safe-mute.conf" "Environment=TSPI_"
  a_contains "$BOARD/etc/tspi-rt-stack/90-safe-mute.env" "TSPI_HOST_SAFE_MODE=mute"
  a_not_contains "$BOARD/etc/tspi-rt-stack/90-safe-mute.env" "TSPI_RT_HOST_SAFE_MODE"
  a_file "$TMP/ev-rb-mute/rollback-report.json"
  a_contains "$TMP/ev-rb-mute/rollback-report.json" "DEGRADED_MUTE"
  a_contains "$TMP/ev-rb-mute/rollback-report.json" "已生效"
  a_log_no_c_start
}
case_atomic_success() {
  reset_board; make_window "$WIN" valid; make_bundle t-001; a_rc 0 $? "bundle"
  do_switch t-001 "$TMP/ev-switch"; a_rc 0 $? "atomic-switch 成功路径"
  a_file "$TMP/ev-switch/result.json"
  a_json "$TMP/ev-switch/result.json" "d['final_state']=='RUNNING_LAST_GOOD' and d['exit']==0"
  a_file "$BOARD/var/lib/tspi-rt-stack/last-good.json"
  a_json "$BOARD/var/lib/tspi-rt-stack/last-good.json" "d['schema']=='tspi-rt-stack/last-good/v1' and d['c_policy']=='never-camilladsp' and d['release_id']=='t-001'"
  a_file "$TMP/ev-switch/verify/verify-report.json"
  a_json "$TMP/ev-switch/verify/verify-report.json" "d['fails']==0"
  a_contains "$MOCK_LOG" "enable tspi-rt-stack.target"
  a_contains "$MOCK_STATE/enabled/tspi-camilladsp.service" "masked"
  a_file "$MOCK_STATE/active/khaen-rt-host.service"
  # F-18/T28-P0-a：start target 后 host+daemon 都必须 active（target Wants= 生效）。
  a_file "$MOCK_STATE/active/tspi-rt-daemon.service"
  a_file "$BOARD/etc/tspi-rt-stack/rt-host.toml"
  a_file "$BOARD/etc/tspi-rt-stack/rt-daemon.toml"
  # F-40：只读 pre-install 门必须排在 install 之前；F-45：stages 与文件头顺序一致。
  local npre nins; npre="$(grep -n '^2-preflight-pre' "$TMP/ev-switch/stages.log" | head -n1 | cut -d: -f1)"; nins="$(grep -n '^3-install' "$TMP/ev-switch/stages.log" | head -n1 | cut -d: -f1)"
  [ -n "$npre" ] && [ -n "$nins" ] && [ "$npre" -lt "$nins" ] || fail_msg "stages 顺序错误：pre-install 应在 install 之前（pre=$npre install=$nins）"
  # task-192：05 在 start target 前显式 start 钉核 oneshot；06 V10 复核 IRQ 仍在 A53
  a_contains "$MOCK_LOG" "start tspi-irq-affinity.service"
  a_eq "1" "$(cat "$BOARD/proc/irq/58/smp_affinity_list")" "切换后 IRQ58 仍在 CPU1"
  a_file "$TMP/ev-switch/irq-affinity.json"
  a_json "$TMP/ev-switch/verify/verify-report.json" "any(c['id']=='V10.pins' and c['status']=='PASS' for c in d['checks'])"
  a_json "$TMP/ev-switch/verify/verify-report.json" "any(c['id']=='V10.unit' and c['status']=='PASS' for c in d['checks'])"
  a_json "$TMP/ev-switch/verify/verify-report.json" "any(c['id']=='V10.irqbalance' and c['status']=='PASS' for c in d['checks'])"
  # B1（2026-11-20 板窗）：重启持久性门——开机等待前置已装、无遗留 live-only drop-in
  a_json "$TMP/ev-switch/verify/verify-report.json" "any(c['id']=='V10.bootwait' and c['status']=='PASS' for c in d['checks'])"
  a_json "$TMP/ev-switch/verify/verify-report.json" "any(c['id']=='V10.legacy-dropin' and c['status']=='PASS' for c in d['checks'])"
  # B1 负例：删掉已装 unit 的 ExecStartPre wait → 06 必须等价重启后 FAIL（rc=5，V10.bootwait FAIL）
  sed -i '/^ExecStartPre=.*irq-affinity-pin wait$/d' "$BOARD/etc/systemd/system/tspi-irq-affinity.service"
  bash "$DEPLOY/bin/06-verify-switch.sh" --live --root "$ROOT" --settle-secs 0 --require-daemon --evidence "$(winpath "$TMP/ev-bootwait-bad")" >"$LOGD/bootwait-bad.log" 2>&1
  a_rc 5 $? "B1：缺开机等待前置时 verify 应 rc=5"
  a_contains "$TMP/ev-bootwait-bad/verify-report.json" "V10.bootwait"
  a_log_no_c_start
}
case_atomic_fail_start() {
  reset_board; make_window "$WIN" valid; make_bundle t-001; a_rc 0 $? "bundle"
  export MOCK_FAIL_MATCH="start tspi-rt-stack.target"
  do_switch t-001 "$TMP/ev-switch-fail"; a_rc 10 $? "启动失败应回退为 mute(10)"
  a_json "$TMP/ev-switch-fail/result.json" "d['final_state']=='DEGRADED_MUTE' and d['exit']==10"
  a_file "$BOARD/etc/systemd/system/khaen-rt-host.service.d/90-safe-mute.conf"
  a_json "$TMP/ev-switch-fail/rollback/rollback-report.json" "d['final_state']=='DEGRADED_MUTE'"
  a_log_no_c_start
  export MOCK_FAIL_MATCH=""
}
case_restore_last_good() {
  reset_board; make_window "$WIN" valid; make_bundle t-001; a_rc 0 $? "bundle1"
  do_switch t-001 "$TMP/ev-sw2"; a_rc 0 $? "首次切换"
  make_bundle t-002; a_rc 0 $? "bundle2"
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$BUNDLE" --evidence "$(winpath "$TMP/ev-inst2")" >"$LOGD/inst2.log" 2>&1
  a_rc 0 $? "安装 t-002"
  bash "$DEPLOY/bin/08-rollback.sh" --live --root "$ROOT" --authorize "$WIN" --evidence "$(winpath "$TMP/ev-rb2")" --to-last-good >"$LOGD/rb2.log" 2>&1
  a_rc 0 $? "回退到 last-good"
  local rel; rel="$(current_release_dir 2>/dev/null || true)"
  case "$rel" in *t-001*) : ;; *) fail_msg "current 未回退到 t-001: $rel" ;; esac
  a_json "$TMP/ev-rb2/rollback-report.json" "d['final_state']=='ROLLED_BACK_LAST_GOOD'"
  a_log_no_c_start
}
case_rollback_contract() {
  reset_board
  mkdir -p "$TMP/reg/bin" "$TMP/reg/releases/t/r1/bin"
  make_elf_aarch64 "$TMP/reg/releases/t/r1/bin/khaen-rt-host"
  printf '{"schema":"tspi-rt-stack/release/v1","release_id":"r1","files":[]}\n' > "$TMP/reg/releases/t/r1/manifest.json"
  printf '{"schema":"tspi-rt-stack/last-good/v1","release_id":"r1","release_dir":"%s","binary_sha256":{"khaen-rt-host":"%s"},"units_sha256":{"khaen-rt-host.service":"%s"},"verified_at":"2026-11-17T00:00:00+08:00","c_policy":"never-camilladsp","previous":null}\n' \
    "$(winpath "$TMP/reg/releases/t/r1")" "$(sha256sum "$TMP/reg/releases/t/r1/bin/khaen-rt-host" | awk '{print $1}')" "$(sha256sum "$TMP/reg/releases/t/r1/manifest.json" | awk '{print $1}')" > "$TMP/reg/last-good.json"
  python "$HERE/check_rollback_file.py" --deploy "$(winpath "$DEPLOY")" --service "$(winpath "$RS/service")" --last-good "$(winpath "$TMP/reg/last-good.json")" --require-release >"$LOGD/rb-contract.log" 2>&1
  a_rc 0 $? "rollback 契约检查 PASS"
  cp -r "$DEPLOY" "$TMP/bad-deploy"
  printf 'systemctl start tspi-camilladsp.service\n' >> "$TMP/bad-deploy/bin/03-retire-c.sh"
  python "$HERE/check_rollback_file.py" --deploy "$(winpath "$TMP/bad-deploy")" --service "$(winpath "$RS/service")" >"$LOGD/rb-contract-bad.log" 2>&1
  a_rc 3 $? "注入 C start 后检查器应 FAIL(3)"
}
case_unit_syntax() {
  python "$HERE/check_unit_syntax.py" "$(winpath "$RS/service")" --json "$(winpath "$TMP/units-report.json")" >"$LOGD/units.log" 2>&1
  a_rc 0 $? "service 单元语法检查"
  a_file "$TMP/units-report.json"
  a_json "$TMP/units-report.json" "d['ok'] is True and d['checked']>=3"
  a_contains "$TMP/units-report.json" "tspi-irq-affinity.service"
  # F3/F4（2026-11-18 板窗缺陷回归）
  a_contains "$RS/service/khaen-rt-host.service" "RuntimeDirectory=tspi-rt-stack"
  a_contains "$RS/service/khaen-rt-host.service" "LogsDirectory=tspi-rt-stack"
  a_contains "$RS/service/khaen-rt-host.service" "DeviceAllow=char-alsa rw"
  a_not_contains "$RS/service/khaen-rt-host.service" "DeviceAllow=/dev/snd/*"
  a_contains "$RS/service/tspi-rt-daemon.service" "RuntimeDirectory=tspi-rt-stack"
  a_contains "$RS/service/tspi-rt-daemon.service" "LogsDirectory=tspi-rt-stack"
  rm -rf "$TMP/bad-units"; cp -r "$RS/service" "$TMP/bad-units"
  printf 'BogusKey=1\n' >> "$TMP/bad-units/khaen-rt-host.service"
  python "$HERE/check_unit_syntax.py" "$(winpath "$TMP/bad-units")" >"$LOGD/units-bad.log" 2>&1
  a_rc 3 $? "未知 key 应 FAIL(3)"
  # F4 负例：DeviceAllow 改回 /dev/snd/* 通配必须 FAIL
  rm -rf "$TMP/bad-units2"; cp -r "$RS/service" "$TMP/bad-units2"
  sed -i 's|DeviceAllow=char-alsa rw|DeviceAllow=/dev/snd/* rw|' "$TMP/bad-units2/khaen-rt-host.service"
  python "$HERE/check_unit_syntax.py" "$(winpath "$TMP/bad-units2")" >"$LOGD/units-bad2.log" 2>&1
  a_rc 3 $? "F4：DeviceAllow=/dev/snd/* 通配应 FAIL(3)"
  # F3 负例：删除 RuntimeDirectory 必须 FAIL
  rm -rf "$TMP/bad-units3"; cp -r "$RS/service" "$TMP/bad-units3"
  sed -i '/^RuntimeDirectory=tspi-rt-stack$/d' "$TMP/bad-units3/khaen-rt-host.service"
  python "$HERE/check_unit_syntax.py" "$(winpath "$TMP/bad-units3")" >"$LOGD/units-bad3.log" 2>&1
  a_rc 3 $? "F3：缺 RuntimeDirectory 应 FAIL(3)"
  # B1（2026-11-20 板窗）负例：删掉开机等待 ExecStartPre → FAIL(3)
  rm -rf "$TMP/bad-units4"; cp -r "$RS/service" "$TMP/bad-units4"
  sed -i '/^ExecStartPre=.*irq-affinity-pin wait$/d' "$TMP/bad-units4/tspi-irq-affinity.service"
  python "$HERE/check_unit_syntax.py" "$(winpath "$TMP/bad-units4")" >"$LOGD/units-bad4.log" 2>&1
  a_rc 3 $? "B1：删除 ExecStartPre wait 应 FAIL(3)"
  # B1 负例：去掉 TimeoutStartSec（等待窗口余量）→ FAIL(3)
  rm -rf "$TMP/bad-units5"; cp -r "$RS/service" "$TMP/bad-units5"
  sed -i '/^TimeoutStartSec=120$/d' "$TMP/bad-units5/tspi-irq-affinity.service"
  python "$HERE/check_unit_syntax.py" "$(winpath "$TMP/bad-units5")" >"$LOGD/units-bad5.log" 2>&1
  a_rc 3 $? "B1：缺 TimeoutStartSec 应 FAIL(3)"
}
case_check_elf() {
  make_elf_aarch64 "$TMP/bin-aarch64"
  python "$DEPLOY/lib/check_elf.py" "$(winpath "$TMP/bin-aarch64")" --require-arch aarch64 >"$LOGD/elf-aarch64.log" 2>&1
  a_rc 0 $? "ELF aarch64 通过"
  make_pe_x86_64 "$TMP/bin-pe"
  python "$DEPLOY/lib/check_elf.py" "$(winpath "$TMP/bin-pe")" --require-arch aarch64 >"$LOGD/elf-pe.log" 2>&1
  a_rc 3 $? "PE 充当 aarch64 应 FAIL(3)"
}
case_bundle_dryrun() {
  mkdir -p "$TMP/bin-dry"
  make_elf_aarch64 "$TMP/bin-dry/khaen-rt-host"
  bash "$DEPLOY/bin/make-release-bundle.sh" --dry-run --bin-dir "$(winpath "$TMP/bin-dry")" --out "$(winpath "$TMP/bundle-dry")" --release-id dry >"$LOGD/bundle-dry.log" 2>&1
  a_rc 0 $? "bundle --dry-run"
  a_absent "$TMP/bundle-dry/manifest.json"
}

case_irq_pin_script() {
  reset_board
  local cfg="$BOARD/etc/tspi-rt-stack/irq-affinity.env"
  printf 'TSPI_IRQ_PINS="58:1 56:2 57:3"\nTSPI_IRQ_DSP_CPUS="4,5,6,7"\n' > "$cfg"
  bash "$DEPLOY/config/irq-affinity-pin" verify --root "$ROOT" >"$LOGD/irqpin-before.log" 2>&1
  a_rc 3 $? "初始 A72 亲和时 pin verify 应 rc=3"
  bash "$DEPLOY/config/irq-affinity-pin" apply --root "$ROOT" >"$LOGD/irqpin-apply.log" 2>&1
  a_rc 0 $? "pin apply"
  a_eq "1" "$(cat "$BOARD/proc/irq/58/smp_affinity_list")" "IRQ58 -> CPU1"
  a_eq "2" "$(cat "$BOARD/proc/irq/56/smp_affinity_list")" "IRQ56 -> CPU2"
  a_eq "3" "$(cat "$BOARD/proc/irq/57/smp_affinity_list")" "IRQ57 -> CPU3"
  python "$DEPLOY/lib/irq_affinity.py" check --root "$(winpath "$BOARD")" --json "$(winpath "$TMP/ev-irq-check.json")" >"$LOGD/irq-check.log" 2>&1
  a_rc 0 $? "helper check（A53 + T45 IRQ 前置）"
  a_json "$TMP/ev-irq-check.json" "d['overall_ok'] is True and d['t45']['plan_equivalent']=='PASS'"
  # 模拟 irqbalance 搬回（task-189 实测 58→7、56→3、57→5）→ helper 与 pin verify 都必须 FAIL
  for pair in 58:7 56:3 57:5; do
    printf '%s\n' "${pair##*:}" > "$BOARD/proc/irq/${pair%%:*}/smp_affinity_list"
  done
  python "$DEPLOY/lib/irq_affinity.py" check --root "$(winpath "$BOARD")" >"$LOGD/irq-check-bad.log" 2>&1
  a_rc 3 $? "IRQ 回 A72 时 helper 应 rc=3"
  bash "$DEPLOY/config/irq-affinity-pin" verify --root "$ROOT" >"$LOGD/irqpin-bad.log" 2>&1
  a_rc 3 $? "IRQ 回 A72 时 pin verify 应 rc=3"
}

case_irq_persist_regression() {
  reset_board; make_window "$WIN" valid; make_bundle t-001; a_rc 0 $? "bundle"
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$BUNDLE" --evidence "$(winpath "$TMP/ev-irq-inst")" >"$LOGD/irq-inst.log" 2>&1
  a_rc 0 $? "04 安装（含停 irqbalance + 写 pin）"
  bash "$DEPLOY/bin/03-retire-c.sh" --live --root "$ROOT" --authorize "$WIN" --evidence "$(winpath "$TMP/ev-irq-retire")" >"$LOGD/irq-retire.log" 2>&1
  a_rc 0 $? "03 退役（verify 需 C 已退役）"
  systemctl start khaen-rt-host.service >/dev/null 2>&1
  bash "$DEPLOY/bin/06-verify-switch.sh" --live --root "$ROOT" --settle-secs 0 --evidence "$(winpath "$TMP/ev-irq-verify-ok")" >"$LOGD/irq-verify-ok.log" 2>&1
  a_rc 0 $? "基线 verify（IRQ 已钉 A53）应 rc=0"
  # 模拟 irqbalance 复活并搬回依赖 IRQ（task-189 实测 58→7、56→3、57→5）
  printf 'enabled\n' > "$MOCK_STATE/enabled/irqbalance.service"
  touch "$MOCK_STATE/active/irqbalance.service"
  for pair in 58:7 56:3 57:5; do
    printf '%s\n' "${pair##*:}" > "$BOARD/proc/irq/${pair%%:*}/smp_affinity_list"
  done
  bash "$DEPLOY/bin/06-verify-switch.sh" --live --root "$ROOT" --settle-secs 0 --evidence "$(winpath "$TMP/ev-irq-verify-bad")" >"$LOGD/irq-verify-bad.log" 2>&1
  a_rc 5 $? "IRQ 被搬回 + irqbalance 复活时 verify 应 rc=5"
  a_contains "$TMP/ev-irq-verify-bad/verify-report.json" "V10.irqbalance"
  a_contains "$TMP/ev-irq-verify-bad/verify-report.json" "V10.pins"
  bash "$DEPLOY/bin/01-preflight-switch.sh" --live --root "$ROOT" --authorize "$WIN" --allow-first-switch >"$LOGD/irq-pf-bad.log" 2>&1
  a_rc 3 $? "IRQ/irqbalance 不满足时 full preflight 应 rc=3"
  a_contains "$LOGD/irq-pf-bad.log" "irqbalance"
  a_contains "$LOGD/irq-pf-bad.log" "IRQ"
  # B1（2026-11-20 板窗）：J 门必须评估开机竞态防护（本次安装为正常路径 → PASS 记录）
  a_contains "$LOGD/irq-pf-bad.log" "开机竞态防护已安装"
}


# ---- 2026-11-20 B1 开机 IRQ 竞态（board-window-20261120 现场 drop-in 固化） ----
case_irq_boot_wait() {
  reset_board
  local pin="$DEPLOY/config/irq-affinity-pin"
  printf 'TSPI_IRQ_PINS="58:1 56:2 57:3"\nTSPI_IRQ_WAIT_SECS="5"\n' > "$BOARD/etc/tspi-rt-stack/irq-affinity.env"
  # 1) 节点齐全：wait 立即 PASS
  TSPI_IRQ_WAIT_SECS=5 bash "$pin" wait --root "$ROOT" >"$LOGD/bootwait-ok.log" 2>&1
  a_rc 0 $? "节点齐全时 pin wait 应 rc=0"
  a_contains "$LOGD/bootwait-ok.log" "RESULT PASS（action=wait"
  # 2) IRQ56 节点晚到 0.6s：wait 轮询等待后 PASS（v11 首启现场：end0 枚举晚于 oneshot）
  rm -f "$BOARD/proc/irq/56/smp_affinity_list"
  ( sleep 0.6; printf '6\n' > "$BOARD/proc/irq/56/smp_affinity_list" ) &
  local bg=$!
  TSPI_IRQ_WAIT_SECS=5 bash "$pin" wait --root "$ROOT" >"$LOGD/bootwait-late.log" 2>&1
  a_rc 0 $? "IRQ 节点晚到时 pin wait 应等待后 rc=0"
  a_contains "$LOGD/bootwait-late.log" "WAIT 依赖 IRQ 节点未就绪"
  wait "$bg" 2>/dev/null || true
  # 3) 节点始终缺失：超时 fail-closed rc=3，日志点名缺失 IRQ
  rm -f "$BOARD/proc/irq/56/smp_affinity_list"
  TSPI_IRQ_WAIT_SECS=1 bash "$pin" wait --root "$ROOT" >"$LOGD/bootwait-timeout.log" 2>&1
  a_rc 3 $? "IRQ 节点始终缺失时 pin wait 应 fail-closed rc=3"
  a_contains "$LOGD/bootwait-timeout.log" "FAIL 依赖 IRQ 的 /proc/irq 节点在 1s 内未就绪"
  a_contains "$LOGD/bootwait-timeout.log" "IRQ56"
  # 4) TSPI_IRQ_WAIT_SECS=0：只查一次立即 rc=3（不等待）
  TSPI_IRQ_WAIT_SECS=0 bash "$pin" wait --root "$ROOT" >"$LOGD/bootwait-zero.log" 2>&1
  a_rc 3 $? "TSPI_IRQ_WAIT_SECS=0 且节点缺失应 rc=3（不等待）"
  # 5) 非法等待值：用法错误 rc=2
  TSPI_IRQ_WAIT_SECS=abc bash "$pin" wait --root "$ROOT" >"$LOGD/bootwait-badsecs.log" 2>&1
  a_rc 2 $? "非法 TSPI_IRQ_WAIT_SECS 应 rc=2"
  # 6) unit 契约：等待前置 + 超时余量
  a_contains "$RS/service/tspi-irq-affinity.service" "ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait"
  a_contains "$RS/service/tspi-irq-affinity.service" "TimeoutStartSec=120"
  # 7) 配置模板说明等待语义（04 首次安装落盘；既有 env 缺键时脚本默认 60）
  a_contains "$DEPLOY/config/irq-affinity.env.example" "TSPI_IRQ_WAIT_SECS="
}

case_irq_boot_persist() {
  reset_board; make_window "$WIN" valid; make_bundle t-001; a_rc 0 $? "bundle"
  # bundle 内的 oneshot 必须携带 B1 等待前置（打包门）
  a_contains "$BUNDLE/units/tspi-irq-affinity.service" "ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait"
  # 板端存量 live-only drop-in（v11 板窗现场）→ 04 应备份到 evidence 后删除
  mkdir -p "$BOARD/etc/systemd/system/tspi-irq-affinity.service.d"
  printf '[Service]\nExecStartPre=/usr/bin/timeout 60 /bin/sh -c "until [ -e /proc/irq/56/smp_affinity_list ]; do sleep 0.2; done"\n' \
    > "$BOARD/etc/systemd/system/tspi-irq-affinity.service.d/10-wait-irq.conf"
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$BUNDLE" --evidence "$(winpath "$TMP/ev-boot-inst")" >"$LOGD/boot-inst.log" 2>&1
  a_rc 0 $? "04 安装（B1 unit 门 + legacy drop-in 迁移）"
  a_contains "$BOARD/etc/systemd/system/tspi-irq-affinity.service" "ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait"
  a_absent "$BOARD/etc/systemd/system/tspi-irq-affinity.service.d/10-wait-irq.conf"
  a_file "$TMP/ev-boot-inst/migrated-dropins/10-wait-irq.conf"
  a_file "$TMP/ev-boot-inst/migrated-dropins/SHA256SUMS"
  a_json "$TMP/ev-boot-inst/install-record.json" "d['boot_race_fix_b1']['legacy_live_only_dropin']=='migrated'"
  # 负例 A：bundle unit 被截（缺 B1 wait 指令；注释匹配不算）+ manifest 重绑 sha → 04 安装前 fail-closed rc=3
  local badb="$TMP/bundle-bootbad"
  rm -rf "$badb"; cp -r "$BUNDLE" "$badb"
  grep -v '^ExecStartPre=' "$badb/units/tspi-irq-affinity.service" > "$badb/units/.unit.tmp"
  mv -f "$badb/units/.unit.tmp" "$badb/units/tspi-irq-affinity.service"
  python - "$(winpath "$badb")" <<'PYEOF' || fail_msg "重绑 bad bundle manifest 失败"
import hashlib, json, os, sys
b = sys.argv[1]
mp = os.path.join(b, 'manifest.json')
m = json.load(open(mp, encoding='utf-8'))
rel = 'units/tspi-irq-affinity.service'
fp = os.path.join(b, rel)
h = hashlib.sha256(open(fp, 'rb').read()).hexdigest()
for e in m['files']:
    if e['path'] == rel:
        e['sha256'] = h
        e['size'] = os.path.getsize(fp)
open(mp, 'w', encoding='utf-8').write(json.dumps(m, ensure_ascii=False, indent=1) + '\n')
PYEOF
  bash "$DEPLOY/bin/04-install-release.sh" --live --root "$ROOT" --authorize "$WIN" --bundle "$badb" --evidence "$(winpath "$TMP/ev-boot-bad")" >"$LOGD/boot-inst-bad.log" 2>&1
  a_rc 3 $? "bundle 缺 B1 wait 前置时 04 应 fail-closed rc=3"
  a_contains "$LOGD/boot-inst-bad.log" "B1 开机竞态防护缺失"
  a_contains "$BOARD/etc/systemd/system/tspi-irq-affinity.service" "ExecStartPre=/usr/local/lib/tspi/irq-affinity-pin wait"
  # 负例 B：旧 service/（缺 B1 wait）进打包 → make-release-bundle 必须拒绝（--service-dir 指向坏目录）
  local badsvc="$TMP/bad-service"
  rm -rf "$badsvc"; cp -r "$RS/service" "$badsvc"
  sed -i '/^ExecStartPre=.*irq-affinity-pin wait$/d' "$badsvc/tspi-irq-affinity.service"
  bash "$DEPLOY/bin/make-release-bundle.sh" --live --bin-dir "$(winpath "$TMP/bin-t-001")" \
    --service-dir "$(winpath "$badsvc")" --out "$(winpath "$TMP/bundle-bootbad2")" --release-id bad2 >"$LOGD/bundle-bad-svc.log" 2>&1
  a_rc 3 $? "旧 service/ 缺 B1 wait 时打包应 rc=3"
  a_contains "$LOGD/bundle-bad-svc.log" "B1/unit 契约"
}

# ---- 2026-11-18 F1–F5 回归（板窗 FINDINGS；证据 evidence/cursor-fix-deploy-f1f5） ----
case_deploy_package_exec() {  # F1：上传包脚本执行位（归档成员模式）+ 包内 SHA256SUMS
  reset_board; make_bundle t-pkg; a_rc 0 $? "bundle"
  printf '# dry-run fixture upload readme\n' > "$TMP/README-UPLOAD.md"
  local pkg="$TMP/pkg-t-pkg.tgz"
  bash "$DEPLOY/bin/make-deploy-package.sh" --live \
    --deploy-dir "$(winpath "$DEPLOY")" --service-dir "$(winpath "$RS/service")" \
    --bundle "$(winpath "$BUNDLE")" --out "$(winpath "$pkg")" \
    --readme "$(winpath "$TMP/README-UPLOAD.md")" --release-id t-pkg >"$LOGD/pkg.log" 2>&1
  a_rc 0 $? "make-deploy-package --live"
  a_file "$pkg"
  a_file "$pkg.verify.json"
  a_file "$pkg.pack.json"
  # F1 核心断言：归档成员模式（不依赖宿主 chmod；Windows 上 chmod 不改变 tar 模式）
  python - "$(winpath "$pkg")" <<'PYEOF' || fail_msg "F1 归档成员模式断言失败"
import fnmatch, sys, tarfile
pats = ['deploy/bin/*.sh', 'deploy/tools/*.sh', 'deploy/tests/*.sh',
        'deploy/config/irq-affinity-pin', 'deploy/config/motu-48k-keeper',
        'release-bundle/bin/*']
with tarfile.open(sys.argv[1]) as t:
    ms = [m for m in t.getmembers() if m.isfile()]
for p in pats:
    hits = [m for m in ms if fnmatch.fnmatchcase(m.name, p)]
    assert hits, '无匹配成员: ' + p
    for m in hits:
        assert m.mode & 0o111, '缺执行位: %s mode=%s' % (m.name, oct(m.mode))
print('PKG_MODE_OK files=%d' % len(ms))
PYEOF
  # 解包后包级 SHA256SUMS 自校验（79+ 项；sha 与成员内容绑定）
  mkdir -p "$TMP/pkg-x"
  tar -xzf "$pkg" -C "$TMP/pkg-x" || fail_msg "tar xzf 上传包失败"
  ( cd "$TMP/pkg-x" && sha256sum -c SHA256SUMS >/dev/null ) || fail_msg "包内 SHA256SUMS 校验失败"
  a_file "$TMP/pkg-x/deploy/bin/05-atomic-switch.sh"
  a_file "$TMP/pkg-x/deploy/bin/make-deploy-package.sh"
  a_file "$TMP/pkg-x/deploy/lib/release_tar.py"
  a_file "$TMP/pkg-x/service/khaen-rt-host.service"
  a_file "$TMP/pkg-x/release-bundle/manifest.json"
  a_file "$TMP/pkg-x/README-UPLOAD.md"
  # 修复已随包：F2 移走再 mask、F5 /proc 环境核对、F3/F4 unit 键
  a_contains "$TMP/pkg-x/deploy/bin/03-retire-c.sh" "unit_file_move_before_mask"
  a_contains "$TMP/pkg-x/deploy/lib/common.sh" "TSPI_PROC_ROOT"
  a_contains "$TMP/pkg-x/service/khaen-rt-host.service" "DeviceAllow=char-alsa rw"
  a_contains "$TMP/pkg-x/service/khaen-rt-host.service" "RuntimeDirectory=tspi-rt-stack"
}

case_retire_mask_unit_file() {  # F2：/etc/systemd/system 普通 unit 文件 → 备份+移走再 mask
  reset_board; make_window "$WIN" valid
  printf '[Unit]\nDescription=old web shim (fixture)\n[Service]\nExecStart=/usr/local/bin/khaen-audio-web\n' \
    > "$BOARD/etc/systemd/system/khaen-audio-web.service"
  bash "$DEPLOY/bin/03-retire-c.sh" --live --root "$ROOT" --authorize "$WIN" \
    --evidence "$(winpath "$TMP/ev-retire-f2")" >"$LOGD/retire-f2.log" 2>&1
  a_rc 0 $? "F2：普通 unit 文件先备份移走再 mask（03 live）"
  a_absent "$BOARD/etc/systemd/system/khaen-audio-web.service"
  a_file "$TMP/ev-retire-f2/retired-unit-files/khaen-audio-web.service"
  a_contains "$TMP/ev-retire-f2/retired-unit-files/khaen-audio-web.service" "old web shim (fixture)"
  a_file "$TMP/ev-retire-f2/retired-unit-files/khaen-audio-web.service.sha256"
  a_file "$TMP/ev-retire-f2/moved-unit-files.tsv"
  a_contains "$TMP/ev-retire-f2/moved-unit-files.tsv" "khaen-audio-web.service"
  a_eq "masked" "$(cat "$MOCK_STATE/enabled/khaen-audio-web.service" 2>/dev/null || true)" "F2：移走后 mask 应成功"
  a_contains "$TMP/ev-retire-f2/result.txt" "FAILS=0"
  # 负例：若单位未移走直接 mask，真实/mock 都会失败 → 03 必须 exit 5（这里验证 helper 先移走的顺序不可省）
  a_contains "$LOGD/retire-f2.log" "已备份并移走普通 unit 文件"
}

case_env_resolve() {  # F5：unit_env_contains 走 /proc/<MainPID>/environ → EnvironmentFiles
  reset_board
  mkdir -p "$BOARD/proc/4321"
  printf 'TSPI_HOST_SAFE_MODE=mute\0PATH=/usr/bin\0' > "$BOARD/proc/4321/environ"
  touch "$MOCK_STATE/active/khaen-rt-host.service"
  # A) /proc/<MainPID>/environ 含 mute（真实进程环境，主通道）
  bash -c '. "$1/lib/common.sh"; unit_env_contains khaen-rt-host.service "TSPI_HOST_SAFE_MODE=mute"' \
    _ "$(winpath "$DEPLOY")" >/dev/null 2>&1
  a_rc 0 $? "F5-A：/proc/<MainPID>/environ 含 mute 应 PASS"
  # B) 无 /proc environ（进程不可读）→ 回退解析 EnvironmentFiles
  rm -f "$BOARD/proc/4321/environ"
  printf 'TSPI_HOST_SAFE_MODE=mute\n' > "$BOARD/etc/tspi-rt-stack/rt-host.env"
  bash -c '. "$1/lib/common.sh"; unit_env_contains khaen-rt-host.service "TSPI_HOST_SAFE_MODE=mute"' \
    _ "$(winpath "$DEPLOY")" >/dev/null 2>&1
  a_rc 0 $? "F5-B：无 proc 时回退 EnvironmentFiles 含 mute 应 PASS"
  # C) 服务在但无静音（proc/environFiles 均 normal）→ 必须 FAIL（保留判定能力）
  printf 'TSPI_HOST_SAFE_MODE=normal\n' > "$BOARD/etc/tspi-rt-stack/rt-host.env"
  bash -c '. "$1/lib/common.sh"; unit_env_contains khaen-rt-host.service "TSPI_HOST_SAFE_MODE=mute"' \
    _ "$(winpath "$DEPLOY")" >/dev/null 2>&1
  a_rc 1 $? "F5-C：服务在但无 mute 必须 FAIL（不得假阳性）"
}

case_mute_env_runtime() {  # F5：08 --mute 用运行时环境核对，报告 已生效 且留证据通道
  reset_board; make_window "$WIN" valid
  local u
  for u in tspi-camilladsp.service tspi-upmix-autodetect.service tspi-boot-mode.service khaen-audio-web.service; do
    printf 'masked\n' > "$MOCK_STATE/enabled/$u"
  done
  printf 'disabled\n' > "$MOCK_STATE/enabled/tspi-audio.target"
  systemctl stop khaen-audio-web.service >/dev/null 2>&1
  mkdir -p "$BOARD/proc/4321"
  printf 'TSPI_HOST_SAFE_MODE=mute\0PATH=/usr/bin\0' > "$BOARD/proc/4321/environ"
  bash "$DEPLOY/bin/08-rollback.sh" --live --root "$ROOT" --authorize "$WIN" \
    --evidence "$(winpath "$TMP/ev-rb-f5")" --mute >"$LOGD/rb-f5.log" 2>&1
  a_rc 10 $? "F5：--mute 应 exit 10（DEGRADED_MUTE）"
  a_contains "$TMP/ev-rb-f5/rollback-report.json" "mute env: 已生效"
  a_not_contains "$TMP/ev-rb-f5/rollback-report.json" "NOT_APPLIED"
  a_contains "$TMP/ev-rb-f5/rollback-report.json" "/proc/4321/environ"
  a_log_no_c_start
}

# ============================== run ==============================
ALL_CASES="help syntax snapshot_dryrun snapshot_live window_absent window_expired preflight_no_release bundle_build install_release install_bad_arch retire_live verify_detects_c rollback_mute atomic_success atomic_fail_start restore_last_good rollback_contract unit_syntax check_elf bundle_dryrun irq_pin_script irq_persist_regression irq_boot_wait irq_boot_persist deploy_package_exec retire_mask_unit_file env_resolve mute_env_runtime"
echo "== T28 dry-run suite（mock systemd；不连板） tmp=$TMP"
for c in $ALL_CASES; do
  [ -n "$ONLY" ] && [ "$ONLY" != "$c" ] && continue
  run_case "$c"
done
python - "$(winpath "$OUT")" "$PASS" "$FAIL" "$(winpath "$TMP/results.log")" <<'PYEOF' || true
import json,sys,shutil,os,datetime
out,passed,failed,reslog=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]),sys.argv[4]
d={"schema":"tspi-rt-stack/dryrun-tests/v1","suites_passed":passed,"suites_failed":failed,
   "total":passed+failed,"verdict":"PASS" if failed==0 else "FAIL",
   "generated_at":datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec="seconds"),
   "environment":"Git Bash + mocked systemctl/pgrep/fuser/ss/curl; no board access"}
os.makedirs(out,exist_ok=True)
open(os.path.join(out,"dryrun-results.json"),"w",encoding="utf-8").write(json.dumps(d,ensure_ascii=False,indent=1)+"\n")
if os.path.isfile(reslog):
    shutil.copyfile(reslog, os.path.join(out,"dryrun-suite.log"))
PYEOF
echo "== RESULT: PASS=$PASS FAIL=$FAIL（日志 $OUT/dryrun-suite.log）"
[ "$FAIL" -eq 0 ] || exit 3
exit 0
