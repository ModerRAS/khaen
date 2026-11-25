#!/usr/bin/env bash
# T147/T28 make-release-bundle.sh —— 把已构建二进制 + service units + 配置模板打成可部署 release bundle
#
# 产物（schema khaen-rt-stack/release/v1，见 lib/release_manifest.py）：
#   <out>/manifest.json       release_id/built_at/target/files[{path,size,sha256,kind}]
#   <out>/bin/                khaen-rt-host（必需）、khaen-rt-daemon（可选）、khaen-cal（可选，goal-8 校准链 CLI）
#   <out>/units/              service/ 下 *.service/*.target + 90-safe-mute.{conf,env}.example
#                             （B1：打包时对 units 跑 deploy/tests/check_unit_syntax.py 契约检查，并要求
#                              khaen-irq-affinity.service 含 ExecStartPre=irq-affinity-pin wait）
#   <out>/etc/                deploy/config 下 env/config 模板
#   <out>/README.deploy.md    部署/回退摘要（引用 deploy/README + evidence/migration-plan）
#   <out>/SHA256SUMS
#
# 二进制必须通过 ELF aarch64 检查（lib/check_elf.py）；本地打包流程测试可用 --allow-foreign-arch（产物标记 NOT_DEPLOYABLE）。
# F1（2026-11-18）：可选 --tgz FILE 用 lib/release_tar.py 显式写成员模式（bin/* = 0755）并校验；
# 不依赖宿主 chmod（Windows/Git Bash 上 chmod 不改变 tar 模式，曾导致上传包脚本 0664/rc=126）。
# 用法:
#   ./make-release-bundle.sh --bin-dir target/aarch64-unknown-linux-musl/release --out /var/tmp/khaen-release-<id> --live
#   ./make-release-bundle.sh --bin-dir DIR --out DIR [--release-id ID] [--tgz FILE] [--service-dir DIR] [--dry-run]
# 退出码: 0 成功；2 用法/缺文件；3 二进制架构/输入校验失败；7 写失败。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"

BIN_DIR=""
OUT=""
SERVICE_DIR=""
RELEASE_ID=""
TARGET="aarch64-unknown-linux-musl"
ALLOW_FOREIGN=0
FORCE=0
TGZ=""
usage() { sed -n '2,25p' "$0" | sed 's/^# \\{0,1\\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --release-id) RELEASE_ID="$2"; shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    --allow-foreign-arch) ALLOW_FOREIGN=1; shift ;;
    --force) FORCE=1; shift ;;
    --tgz) TGZ="$2"; shift 2 ;;
    --service-dir) SERVICE_DIR="$2"; shift 2 ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

[ -n "$BIN_DIR" ] || die "缺少 --bin-dir（cargo release 输出目录）" 2
[ -n "$OUT" ] || die "缺少 --out（bundle 输出目录）" 2
[ -n "$RELEASE_ID" ] || RELEASE_ID="local-$(date +%Y%m%dT%H%M%S)"
HOST_BIN="$BIN_DIR/khaen-rt-host"
[ -f "$HOST_BIN" ] || die "缺少 khaen-rt-host: $HOST_BIN（先完成 aarch64 构建；见 evidence/deploy-artifacts）" 2

# ---- 二进制架构/哈希 ---------------------------------------------------------
log "host binary: size=$(file_size_of "$HOST_BIN") sha256=$(sha256_of "$HOST_BIN")"
ARCH_JSON="$(py "$DEPLOY_ROOT/lib/check_elf.py" "$HOST_BIN" 2>/dev/null || true)"
log "arch: $(printf '%s' "$ARCH_JSON" | py -c 'import json,sys;d=json.load(sys.stdin);print(d.get("format"),d.get("arch"),d.get("bits"))' 2>/dev/null || printf '%s' "$ARCH_JSON")"
if [ "$ALLOW_FOREIGN" != "1" ]; then
  py "$DEPLOY_ROOT/lib/check_elf.py" "$HOST_BIN" --require-arch aarch64 >/dev/null || die "khaen-rt-host 不是 ELF aarch64（可 --allow-foreign-arch 做本地流程测试，但产物 NOT_DEPLOYABLE）" 3
fi
BINS=("$HOST_BIN")
[ -f "$BIN_DIR/khaen-rt-daemon" ] && BINS+=("$BIN_DIR/khaen-rt-daemon")
# goal-8 / aarch64-20261122：校准链 CLI（crates/calibration per goal-8）随包发布；缺失不影响 host/daemon 打包。
[ -f "$BIN_DIR/khaen-cal" ] && BINS+=("$BIN_DIR/khaen-cal")

UNITS_DIR="${SERVICE_DIR:-$DEPLOY_ROOT/../service}"
[ -d "$UNITS_DIR" ] || die "缺 service/ 目录: $UNITS_DIR（缺省 deploy/../service；可用 --service-dir 指定）" 2
ETC_DIR="$DEPLOY_ROOT/config"

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] bundle 输出目录: $OUT（release_id=$RELEASE_ID target=$TARGET）"
  log "[dry-run] bin: ${BINS[*]}"
  log "[dry-run] units: $(cd "$UNITS_DIR" && printf '%s ' *.service *.target) 90-safe-mute.conf.example 90-safe-mute.env.example"
  log "[dry-run] units 契约检查: py $DEPLOY_ROOT/tests/check_unit_syntax.py <out>/units（B1：khaen-irq-affinity.service 必须含 ExecStartPre=irq-affinity-pin wait）"
  log "[dry-run] etc: $(cd "$ETC_DIR" && printf '%s ' *.example)"
  log "[dry-run] manifest.json（逐文件 size/sha256）+ README.deploy.md + SHA256SUMS"
  exit 0
fi

if [ -e "$OUT" ] && [ "$FORCE" != "1" ]; then
  [ -z "$(ls -A "$OUT" 2>/dev/null)" ] || die "输出目录非空（--force 覆盖）: $OUT" 2
fi
mkdir -p "$OUT/bin" "$OUT/units" "$OUT/etc" || die "无法创建 bundle 目录: $OUT" 7

install -m 0755 "${BINS[@]}" "$OUT/bin/" || die "复制二进制失败" 7
for f in "$UNITS_DIR"/*.service "$UNITS_DIR"/*.target; do
  [ -f "$f" ] || continue
  install -m 0644 "$f" "$OUT/units/"
done
for f in khaen-rt-host.service.d/90-safe-mute.conf.example khaen-rt-host.service.d/90-safe-mute.env.example; do
  [ -f "$UNITS_DIR/$f" ] && install -m 0644 "$UNITS_DIR/$f" "$OUT/units/"
done || true
# B1（2026-11-20 板窗）：units 必须过 systemd 契约检查（含 oneshot 开机等待前置），
# 防止旧 service/ 进 bundle 后重启再触发 IRQ 竞态（host 因 Requires= 不启动）。
UNIT_CHECK="$OUT/.unit-syntax.json"
if ! py "$DEPLOY_ROOT/tests/check_unit_syntax.py" "$OUT/units" >"$UNIT_CHECK" 2>&1; then
  warn "units 契约检查 FAIL：$(tail -n 8 "$UNIT_CHECK" | tr '\n' ' ')"
  rm -f "$UNIT_CHECK"
  die "bundle units 未通过 check_unit_syntax.py（B1/unit 契约；用现行 service/ 重打）" 3
fi
rm -f "$UNIT_CHECK"
if ! grep -q '^ExecStartPre=/usr/local/lib/khaen/irq-affinity-pin wait' "$OUT/units/khaen-irq-affinity.service"; then
  die "bundle 的 khaen-irq-affinity.service 缺 B1 开机等待 ExecStartPre=irq-affinity-pin wait（用现行 service/ 重打）" 3
fi
log "units 契约检查 PASS（B1 ExecStartPre wait 已包含）"
for f in "$ETC_DIR"/*.example; do
  [ -f "$f" ] || continue
  install -m 0640 "$f" "$OUT/etc/"
done

cat > "$OUT/README.deploy.md" <<MD
# KHAEN RT-first DSP stack release（$RELEASE_ID）

> 由 deploy/bin/make-release-bundle.sh 生成；部署/切换/回退见 rt-stack/deploy/README.md 与
> rt-stack/evidence/migration-plan/PLAN.md。P1 G-no-C：禁止把 CamillaDSP/旧 C 任何 unit 作为回退。

- target: $TARGET
- built_at: $(now_iso)
- bundle: bin/（${#BINS[@]} 个二进制）、units/、etc/、manifest.json
- 安装：deploy/bin/04-install-release.sh --live --authorize <window.json> --bundle <本目录>
- 切换：deploy/bin/05-atomic-switch.sh --live --authorize <window.json> --bundle <本目录>
- 回退：deploy/bin/08-rollback.sh --live --authorize <window.json>（只回自研 last-good 或 mute）
MD

py - "$OUT" "$RELEASE_ID" "$TARGET" "${BINS[@]}" <<'PY' || die "生成 manifest.json 失败" 7
import hashlib, json, os, sys, datetime
out, rid, target = sys.argv[1], sys.argv[2], sys.argv[3]
bins = sys.argv[4:]
files = []
def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for c in iter(lambda: f.read(1 << 20), b''):
            h.update(c)
    return h.hexdigest()
def add(rel, kind):
    fp = os.path.join(out, rel)
    if os.path.isfile(fp):
        files.append({'path': rel, 'size': os.path.getsize(fp), 'sha256': sha(fp), 'kind': kind})
for b in bins:
    add('bin/' + os.path.basename(b), 'binary')
for d, kind in (('units', 'unit'), ('etc', 'config')):
    p = os.path.join(out, d)
    for n in sorted(os.listdir(p)):
        add(d + '/' + n, kind)
add('README.deploy.md', 'doc')
m = {'schema': 'khaen-rt-stack/release/v1', 'release_id': rid,
     'built_at': datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec='seconds'),
     'target': target, 'files': files}
open(os.path.join(out, 'manifest.json'), 'w', encoding='utf-8').write(json.dumps(m, ensure_ascii=False, indent=1) + '\n')
print(json.dumps({'ok': True, 'release_id': rid, 'files': len(files)}, ensure_ascii=False))
PY

( cd "$OUT" && find bin units etc README.deploy.md manifest.json -type f | sort | xargs sha256sum > SHA256SUMS )
log "bundle 完成: $OUT（release_id=$RELEASE_ID）"
log "校验: py $DEPLOY_ROOT/lib/release_manifest.py verify $OUT --require-arch aarch64"
# F1（2026-11-18）：可选打 tgz——release_tar.py 显式写成员模式（bin/* = 0755）并做包后校验，
# 不依赖宿主 chmod/tar（Windows/Git Bash 上 chmod 不改变归档模式）。
if [ -n "$TGZ" ]; then
  mkdir -p "$(dirname "$TGZ")" || die "创建 tgz 目录失败: $(dirname "$TGZ")" 7
  py "$DEPLOY_ROOT/lib/release_tar.py" pack --root "$OUT" --out "$TGZ" --exec 'bin/*' \
     --json "$TGZ.pack.json" >/dev/null || die "bundle tgz 打包失败: $TGZ" 7
  py "$DEPLOY_ROOT/lib/release_tar.py" verify --archive "$TGZ" --require-exec 'bin/*' \
     --sums "$OUT/SHA256SUMS" --json "$TGZ.verify.json" >/dev/null || die "bundle tgz 校验失败: $TGZ" 3
  log "bundle tgz: $TGZ（size=$(file_size_of "$TGZ") sha256=$(sha256_of "$TGZ")；成员模式/清单校验 PASS）"
fi
exit 0
