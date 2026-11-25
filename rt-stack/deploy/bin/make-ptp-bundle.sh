#!/usr/bin/env bash
# TSPI PTPv2 grandmaster add-on bundle packer (aarch64).
#
# Packs the pinned linuxptp binaries + the PTP units/config/wrappers from rt-stack/ptp/ into a
# release bundle that sits next to deploy/aarch64-20261122/ (base rt-stack release):
#   <out>/bin/{ptp4l,phc2sys,pmc,phc_ctl}
#   <out>/lib/{ptp-lib.sh,ptp-master-start,ptp-phc2sys-start}
#   <out>/units/{tspi-ptp-master.service,tspi-ptp-phc2sys.service,tspi-ptp.target}
#   <out>/etc/{ptp-master.env.example,ptp4l-master-udp.conf.example,ptp4l-master-l2.conf.example}
#   <out>/README.deploy.md, <out>/INSTALL-ROLLBACK.md, <out>/manifest.json, <out>/SHA256SUMS
#   optional --tgz FILE (explicit member modes via lib/release_tar.py)
#
# Gates: ELF aarch64 per binary, check_unit_syntax.py on units/, check_ptp_bundle.py on the tree,
#       release_manifest.py verify --require-arch aarch64, tgz member-mode + SHA256SUMS verify.
# Usage:
#   ./make-ptp-bundle.sh --bin-dir DIR --out DIR [--release-id ID] [--tgz FILE] [--force]
#                        [--allow-foreign-arch] [--live|--dry-run]
# Exit: 0 OK; 2 usage/input; 3 validation FAIL; 7 write FAIL.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"

BIN_DIR=""; OUT=""; RELEASE_ID=""; TGZ=""; FORCE=0; ALLOW_FOREIGN=0
PTP_ROOT="$(cd "$DEPLOY_ROOT/../ptp" && pwd)"
usage() { sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --release-id) RELEASE_ID="$2"; shift 2 ;;
    --tgz) TGZ="$2"; shift 2 ;;
    --force) FORCE=1; shift ;;
    --allow-foreign-arch) ALLOW_FOREIGN=1; shift ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done
[ -n "$BIN_DIR" ] || die "缺少 --bin-dir（aarch64 linuxptp 构建输出目录）" 2
[ -n "$OUT" ] || die "缺少 --out" 2
[ -n "$RELEASE_ID" ] || RELEASE_ID="ptp-aarch64-$(date +%Y%m%dT%H%M%S)"
[ -d "$BIN_DIR" ] || die "bin-dir 不存在: $BIN_DIR" 2
[ -d "$PTP_ROOT/units" ] || die "缺 ptp/units: $PTP_ROOT" 2
BINS=(ptp4l phc2sys pmc phc_ctl)
for b in "${BINS[@]}"; do [ -f "$BIN_DIR/$b" ] || die "缺二进制: $BIN_DIR/$b" 2; done

log "ptp bundle: release_id=$RELEASE_ID target=aarch64-unknown-linux-gnu out=$OUT"
for b in "${BINS[@]}"; do
  log "bin $(basename "$b") size=$(file_size_of "$BIN_DIR/$b") sha256=$(sha256_of "$BIN_DIR/$b")"
  if [ "$ALLOW_FOREIGN" != "1" ]; then
    py "$DEPLOY_ROOT/lib/check_elf.py" "$BIN_DIR/$b" --require-arch aarch64 >/dev/null \
      || die "不是 ELF aarch64: $b（可 --allow-foreign-arch 做本地流程测试）" 3
  fi
done

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 输出: $OUT（bin/lib/units/etc + manifest.json + SHA256SUMS + README.deploy.md + INSTALL-ROLLBACK.md）"
  log "[dry-run] units 契约: py $DEPLOY_ROOT/tests/check_unit_syntax.py $OUT/units"
  log "[dry-run] ptp 契约: py $PTP_ROOT/tests/check_ptp_bundle.py $OUT"
  [ -n "$TGZ" ] && log "[dry-run] tgz: $TGZ（bin/*, lib/ptp-master-start, lib/ptp-phc2sys-start 为 0755）"
  exit 0
fi
if [ -e "$OUT" ] && [ "$FORCE" != "1" ]; then
  [ -z "$(ls -A "$OUT" 2>/dev/null)" ] || die "输出目录非空（--force 覆盖）: $OUT" 2
fi
mkdir -p "$OUT/bin" "$OUT/lib" "$OUT/units" "$OUT/etc" || die "无法创建 $OUT" 7
for b in "${BINS[@]}"; do install -m 0755 "$BIN_DIR/$b" "$OUT/bin/$b" || die "复制 $b 失败" 7; done
install -m 0644 "$PTP_ROOT/lib/ptp-lib.sh" "$OUT/lib/ptp-lib.sh" || die "复制 ptp-lib.sh 失败" 7
install -m 0755 "$PTP_ROOT/lib/ptp-master-start" "$OUT/lib/ptp-master-start" || die "复制 ptp-master-start 失败" 7
install -m 0755 "$PTP_ROOT/lib/ptp-phc2sys-start" "$OUT/lib/ptp-phc2sys-start" || die "复制 ptp-phc2sys-start 失败" 7
for f in "$PTP_ROOT/units/"*.service "$PTP_ROOT/units/"*.target; do
  [ -f "$f" ] || continue
  install -m 0644 "$f" "$OUT/units/" || die "复制 unit 失败: $f" 7
done
for f in "$PTP_ROOT/etc/"*.example; do
  [ -f "$f" ] || continue
  install -m 0644 "$f" "$OUT/etc/" || die "复制 etc 失败: $f" 7
done
install -m 0644 "$PTP_ROOT/INSTALL-ROLLBACK.md" "$OUT/INSTALL-ROLLBACK.md" || die "复制 INSTALL-ROLLBACK.md 失败" 7

cat > "$OUT/README.deploy.md" <<MD
# TSPI PTPv2 grandmaster add-on bundle (${RELEASE_ID})

> 由 deploy/bin/make-ptp-bundle.sh 生成；安装/验证/回滚见同目录 INSTALL-ROLLBACK.md 与
> rt-stack/ptp/README.md、docs/BOARD-PTP-MASTER.md。板端 install/start = NOT_RUN。

- target: aarch64-unknown-linux-gnu（板端 Debian 12 / glibc >= 2.34）
- 内容: bin/{ptp4l,phc2sys,pmc,phc_ctl}、lib/（启动包装）、units/、etc/（配置模板）
- 安装（默认 dry-run；live 需授权窗口 scope=install-ptp-master）:
  \`sudo bash <repo>/rt-stack/deploy/bin/09-install-ptp-gm.sh --live --authorize WINDOW.json --bundle <本目录>\`
- 回滚: \`sudo bash <repo>/rt-stack/deploy/bin/10-rollback-ptp-gm.sh --live --authorize WINDOW.json [--remove]\`
MD
# shellcheck disable=SC2016
sed -i "s/${RELEASE_ID})/$RELEASE_ID/g" "$OUT/README.deploy.md"

python3 - "$OUT" "$RELEASE_ID" <<'PY' || die "生成 manifest.json 失败" 7
import hashlib, json, os, sys, datetime
out, rid = sys.argv[1], sys.argv[2]
kinds = {'bin': 'binary', 'lib': 'lib', 'units': 'unit', 'etc': 'config'}

def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for c in iter(lambda: f.read(1 << 20), b''):
            h.update(c)
    return h.hexdigest()

files = []
for d in ('bin', 'lib', 'units', 'etc'):
    p = os.path.join(out, d)
    if not os.path.isdir(p):
        continue
    for n in sorted(os.listdir(p)):
        fp = os.path.join(p, n)
        if os.path.isfile(fp):
            files.append({'path': d + '/' + n, 'size': os.path.getsize(fp), 'sha256': sha(fp), 'kind': kinds[d]})
for n in ('README.deploy.md', 'INSTALL-ROLLBACK.md'):
    fp = os.path.join(out, n)
    if os.path.isfile(fp):
        files.append({'path': n, 'size': os.path.getsize(fp), 'sha256': sha(fp), 'kind': 'doc'})
m = {'schema': 'tspi-rt-stack/release/v1', 'release_id': rid,
     'built_at': datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec='seconds'),
     'target': 'aarch64-unknown-linux-gnu', 'files': files}
open(os.path.join(out, 'manifest.json'), 'w', encoding='utf-8').write(json.dumps(m, ensure_ascii=False, indent=1) + '\n')
print(json.dumps({'ok': True, 'release_id': rid, 'files': len(files)}, ensure_ascii=False))
PY
( cd "$OUT" && find bin lib units etc README.deploy.md INSTALL-ROLLBACK.md manifest.json -type f | sort | xargs sha256sum > SHA256SUMS ) || die "SHA256SUMS 生成失败" 7

# ---- gates ----
py "$DEPLOY_ROOT/tests/check_unit_syntax.py" "$OUT/units" > "$OUT/.unit-syntax.json" 2>&1 \
  || { tail -n 15 "$OUT/.unit-syntax.json" >&2; rm -f "$OUT/.unit-syntax.json"; die "units 未通过 check_unit_syntax.py" 3; }
rm -f "$OUT/.unit-syntax.json"
py "$PTP_ROOT/tests/check_ptp_bundle.py" "$OUT" > "$OUT/.ptp-check.json" 2>&1 \
  || { cat "$OUT/.ptp-check.json" >&2; rm -f "$OUT/.ptp-check.json"; die "PTP 契约检查 FAIL" 3; }
rm -f "$OUT/.ptp-check.json"
log "units/PTP 契约检查 PASS"
py "$DEPLOY_ROOT/lib/release_manifest.py" verify "$OUT" --require-arch aarch64 >/dev/null \
  || die "release_manifest verify 失败" 3
log "release_manifest verify PASS（--require-arch aarch64）"

if [ -n "$TGZ" ]; then
  mkdir -p "$(dirname "$TGZ")" || die "创建 tgz 目录失败" 7
  py "$DEPLOY_ROOT/lib/release_tar.py" pack --root "$OUT" --out "$TGZ" \
     --exec 'bin/*' --exec 'lib/ptp-master-start' --exec 'lib/ptp-phc2sys-start' \
     --json "$TGZ.pack.json" >/dev/null || die "tgz 打包失败: $TGZ" 7
  py "$DEPLOY_ROOT/lib/release_tar.py" verify --archive "$TGZ" \
     --require-exec 'bin/*' --require-exec 'lib/ptp-master-start' --require-exec 'lib/ptp-phc2sys-start' \
     --sums "$OUT/SHA256SUMS" --json "$TGZ.verify.json" >/dev/null || die "tgz 校验失败: $TGZ" 3
  log "tgz: $TGZ size=$(file_size_of "$TGZ") sha256=$(sha256_of "$TGZ")（成员模式/清单 PASS）"
fi
log "bundle 完成: $OUT"
exit 0
