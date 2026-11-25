#!/usr/bin/env bash
# T28 07-mark-last-good.sh —— 记录自研 last-good（仅成功切换并验证后写；P1 回退唯一目标，另一兜底为 mute）
#
# 写入 /var/lib/khaen-rt-stack/last-good.json（schema khaen-rt-stack/last-good/v1，原子写）：
#   release_id/release_dir/manifest_sha256/binary_sha256/units_sha256/verified_at/verify_report/
#   c_policy=never-camilladsp/previous
# 禁止把 CamillaDSP/旧 C 写进 last-good（G-no-C）；写前校验旧 C 已退役。
#
# 用法: sudo ./07-mark-last-good.sh --live --authorize WINDOW.json [--verify-report FILE] [--release-id ID] [--root DIR]
# 退出码: 0 已记录；3 校验失败（未写）；4 授权失败；7 环境不支持。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"
. "$HERE/../config/old-c-units.conf"

RELEASE_ID=""
VERIFY_REPORT=""
usage() { sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    --evidence) EVIDENCE_DIR="$2"; shift 2 ;;
    --release-id) RELEASE_ID="$2"; shift 2 ;;
    --verify-report) VERIFY_REPORT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

require_authorization "mark-last-good"
[ "$DRY_RUN" = "0" ] && require_root_live
evidence_prepare "07-mark-last-good"

REL_DIR=""
if [ -n "$RELEASE_ID" ]; then REL_DIR="$(p "$KHAEN_STACK_PREFIX")/releases/$RELEASE_ID"; else REL_DIR="$(current_release_dir 2>/dev/null || true)"; fi
[ -n "$REL_DIR" ] || die "找不到 current release（先 04-install-release.sh）" 3
[ -f "$REL_DIR/manifest.json" ] || die "release 缺 manifest.json: $REL_DIR" 3
REL_ID="$(printf '%s' "$RELEASE_ID")"
[ -n "$REL_ID" ] || REL_ID="$(py -c 'import json,sys;print(json.load(open(sys.argv[1],encoding="utf-8"))["release_id"])' "$REL_DIR/manifest.json")"

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 将校验 release=$REL_ID 与 C 退役，写 last-good.json（schema khaen-rt-stack/last-good/v1, c_policy=never-camilladsp）"
  exit 0
fi

# 硬前置：C 已退役；host 已 active；verify 报告 PASS（给了就必须 PASS）
BAD="$(assert_c_retired || true)"
[ -z "$(printf '%s' "$BAD" | tr -d '[:space:]')" ] || die "旧 C 未退役，拒绝写 last-good: $(printf '%s' "$BAD" | tr '\n' ' ')" 3
sunit_active khaen-rt-host.service || die "khaen-rt-host.service 未 active，拒绝写 last-good" 3
if [ -n "$VERIFY_REPORT" ]; then
  [ -f "$VERIFY_REPORT" ] || die "verify report 不存在: $VERIFY_REPORT" 3
  py - "$VERIFY_REPORT" <<'PY' || exit 3
import json,sys
d=json.load(open(sys.argv[1],encoding="utf-8"))
if d.get("schema")!="khaen-rt-stack/verify-report/v1": raise SystemExit("verify-report schema 不符")
if int(d.get("fails",1))!=0: raise SystemExit(f"verify-report fails={d.get('fails')}，拒绝写 last-good")
print("verify-report PASS")
PY
fi
py "$DEPLOY_ROOT/lib/release_manifest.py" verify "$REL_DIR" --require-arch aarch64 >/dev/null || die "release manifest/arch 校验失败" 3

LG="$(p "$KHAEN_STATE_DIR/last-good.json")"
TMP="$LG.tmp.$$"
mkdir -p "$(dirname "$LG")"
py - "$TMP" "$REL_ID" "$REL_DIR" "$(p "$KHAEN_SYSTEMD_DIR")" "${VERIFY_REPORT:-}" "$LG" <<'PY' || die "生成 last-good JSON 失败" 7
import hashlib, json, os, sys, datetime
tmp, rid, rel_dir, sys_dir, verify_report, lg_path = sys.argv[1:7]
def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()
bins = {}
for name in sorted(os.listdir(os.path.join(rel_dir, "bin"))):
    fp = os.path.join(rel_dir, "bin", name)
    if os.path.isfile(fp):
        bins[name] = sha(fp)
units = {}
for name in sorted(os.listdir(sys_dir)):
    if not name.startswith("khaen-rt-"):
        continue
    fp = os.path.join(sys_dir, name)
    if os.path.isfile(fp):
        units[name] = sha(fp)
prev = None
if os.path.isfile(lg_path):
    try:
        old = json.load(open(lg_path, encoding="utf-8"))
        prev = {"release_id": old.get("release_id"), "release_dir": old.get("release_dir")}
    except (OSError, ValueError):
        prev = None
d = {
    "schema": "khaen-rt-stack/last-good/v1",
    "release_id": rid,
    "release_dir": rel_dir,
    "manifest_sha256": sha(os.path.join(rel_dir, "manifest.json")),
    "binary_sha256": bins,
    "units_sha256": units,
    "verified_at": datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec="seconds"),
    "verify_report": verify_report,
    "c_policy": "never-camilladsp",
    "previous": prev,
}
open(tmp, "w", encoding="utf-8").write(json.dumps(d, ensure_ascii=False, indent=1) + "\n")
PY
mv -f "$TMP" "$LG" || die "原子写 last-good 失败: $LG" 7
cp -f "$LG" "$EVIDENCE_DIR/last-good.json" 2>/dev/null || true
log "last-good 已记录: $LG（release=$REL_ID）"
exit 0
