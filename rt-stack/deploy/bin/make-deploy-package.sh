#!/usr/bin/env bash
# T28/F1 make-deploy-package.sh —— 组装板端上传包（deploy/ + service/ + release-bundle/ + README + SHA256SUMS）
#
# 2026-11-18 F1 修复（rt-stack/evidence/board-window-20261118/deploy/FINDINGS.md F1）：
#   * 旧流程在 Windows 构建机上 tar czf，deploy/bin/*.sh 记录为 0664；板端 tar xzf 后直接运行 rc=126。
#   * 现流程：① 组装 staging；② 生成包级 SHA256SUMS；③ 用 lib/release_tar.py **显式写成员模式**
#     （脚本 0755 / 其余 0644，不依赖宿主 chmod）；④ 打包后校验归档成员 exec 位 + SHA256SUMS；
#     ⑤ Linux 上再解包 sha256sum -c + test -x（Windows 只校验归档模式，不做解包 exec 断言）。
#   * 包内脚本执行位以**归档成员模式**为准（跨平台可复现），README §3 同时给 chmod 兜底。
#
# 用法:
#   ./make-deploy-package.sh --bundle DIR --out FILE.tgz [--readme README-UPLOAD.md]
#       [--deploy-dir DIR] [--service-dir DIR] [--release-id ID]
#       [--allow-foreign-arch] [--keep-stage] [--live|--dry-run]
# 退出码: 0 成功；2 用法/缺输入；3 bundle/包校验失败；7 写失败。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"

BUNDLE=""
OUT=""
README=""
DEPLOY_DIR="$DEPLOY_ROOT"
SERVICE_DIR=""
RELEASE_ID=""
ALLOW_FOREIGN=0
KEEP_STAGE=0
usage() { sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle) BUNDLE="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --readme) README="$2"; shift 2 ;;
    --deploy-dir) DEPLOY_DIR="$2"; shift 2 ;;
    --service-dir) SERVICE_DIR="$2"; shift 2 ;;
    --release-id) RELEASE_ID="$2"; shift 2 ;;
    --allow-foreign-arch) ALLOW_FOREIGN=1; shift ;;
    --keep-stage) KEEP_STAGE=1; shift ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --root) ROOT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done

[ -n "$BUNDLE" ] || die "缺少 --bundle（release bundle 目录）" 2
[ -n "$OUT" ] || die "缺少 --out（上传包 tgz 路径）" 2
[ -d "$BUNDLE" ] || die "bundle 不存在: $BUNDLE" 2
[ -d "$DEPLOY_DIR" ] || die "deploy 目录不存在: $DEPLOY_DIR" 2
[ -z "$SERVICE_DIR" ] && SERVICE_DIR="$(cd "$DEPLOY_DIR/.." && pwd)/service"
[ -d "$SERVICE_DIR" ] || die "service 目录不存在: $SERVICE_DIR" 2
if [ -n "$README" ] && [ ! -f "$README" ]; then die "--readme 不存在: $README" 2; fi
OUTDIR="$(cd "$(dirname "$OUT")" 2>/dev/null && pwd || true)"
[ -n "$OUTDIR" ] || OUTDIR="$(pwd)"
OUT="$OUTDIR/$(basename "$OUT")"

# F1 必需执行位的包内路径（release_tar.py 用同一 glob 写入模式并事后校验）
EXEC_PATTERNS=(
  'deploy/bin/*.sh'
  'deploy/tools/*.sh'
  'deploy/tests/*.sh'
  'deploy/config/irq-affinity-pin'
  'deploy/config/motu-48k-keeper'
  'deploy/config/pstore-archive'
  'deploy/config/thermal-log'
  'release-bundle/bin/*'
)
EXEC_ARGS=()
for pat in "${EXEC_PATTERNS[@]}"; do EXEC_ARGS+=(--exec "$pat"); done
VERIFY_EXEC_ARGS=()
for pat in "${EXEC_PATTERNS[@]}"; do VERIFY_EXEC_ARGS+=(--require-exec "$pat"); done

REL_ID="$RELEASE_ID"
if [ -z "$REL_ID" ]; then
  REL_ID="$(py - "$BUNDLE/manifest.json" <<'PY' 2>/dev/null || true
import json,sys
try:
    print(json.load(open(sys.argv[1], encoding="utf-8")).get("release_id") or "")
except Exception:
    print("")
PY
)"
fi
[ -n "$REL_ID" ] || REL_ID="local-$(date +%Y%m%dT%H%M%S)"
log "deploy package: release_id=$REL_ID bundle=$BUNDLE out=$OUT"
log "deploy-dir=$DEPLOY_DIR service-dir=$SERVICE_DIR readme=${README:-none}"

if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 校验 bundle: py $DEPLOY_ROOT/lib/release_manifest.py verify $BUNDLE ${ALLOW_FOREIGN:+--allow-foreign-arch}"
  log "[dry-run] staging: deploy/ + service/ + release-bundle/ + README-UPLOAD.md + SHA256SUMS"
  log "[dry-run] 成员模式由 release_tar.py 显式写入: ${EXEC_PATTERNS[*]} -> 0755；其余 0644"
  log "[dry-run] pack: py $DEPLOY_ROOT/lib/release_tar.py pack --root <stage> --out $OUT ${EXEC_ARGS[*]}"
  log "[dry-run] verify: 归档成员 exec 位 + SHA256SUMS（Linux 追加解包 sha256sum -c / test -x）"
  exit 0
fi

# ---- 1 bundle 语义校验（manifest schema/size/sha256/ELF 架构） --------------------------
VERIFY_ARGS=()
[ "$ALLOW_FOREIGN" = "1" ] || VERIFY_ARGS+=(--require-arch aarch64)
if ! out="$(py "$DEPLOY_ROOT/lib/release_manifest.py" verify "$BUNDLE" "${VERIFY_ARGS[@]}" 2>&1)"; then
  die "release bundle 校验失败（拒绝打包）: $out" 3
fi
log "bundle PASS：$(printf '%s' "$out" | tr '\n' ' ')"

# ---- 2 staging -----------------------------------------------------------------------
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/tspi-deploy-package-XXXXXX")" || die "mktemp 失败" 7
cleanup() { [ "$KEEP_STAGE" = "1" ] || rm -rf "$STAGE"; }
trap cleanup EXIT
mkdir -p "$STAGE/deploy" "$STAGE/service" "$STAGE/release-bundle" || die "创建 staging 失败: $STAGE" 7
cp -a "$DEPLOY_DIR/." "$STAGE/deploy/" || die "复制 deploy/ 失败" 7
cp -a "$SERVICE_DIR/." "$STAGE/service/" || die "复制 service/ 失败" 7
cp -a "$BUNDLE/." "$STAGE/release-bundle/" || die "复制 release-bundle/ 失败" 7
if [ -n "$README" ]; then
  cp -f "$README" "$STAGE/README-UPLOAD.md" || die "复制 README-UPLOAD.md 失败" 7
else
  warn "未给 --readme：上传包不含 README-UPLOAD.md（仅调试用；上板包必须带）"
fi
log "staging: $STAGE"

# ---- 3 包级 SHA256SUMS（相对路径；Linux sha256sum -c 可用） ---------------------------
SUM_TARGETS=(deploy service release-bundle)
[ -f "$STAGE/README-UPLOAD.md" ] && SUM_TARGETS+=(README-UPLOAD.md)
if ! ( cd "$STAGE" && find "${SUM_TARGETS[@]}" -type f | LC_ALL=C sort | xargs sha256sum > SHA256SUMS ); then
  die "生成包级 SHA256SUMS 失败" 7
fi
FILE_COUNT="$(wc -l < "$STAGE/SHA256SUMS" | tr -d '[:space:]')"
log "SHA256SUMS: $FILE_COUNT files"

# ---- 4 打包（显式成员模式）+ 包后校验 ------------------------------------------------
mkdir -p "$(dirname "$OUT")" || die "创建输出目录失败: $(dirname "$OUT")" 7
if ! py "$DEPLOY_ROOT/lib/release_tar.py" pack --root "$STAGE" --out "$OUT" "${EXEC_ARGS[@]}"      --json "$OUT.pack.json" >/dev/null; then
  die "打包失败（release_tar.py pack）: $OUT" 7
fi
if ! py "$DEPLOY_ROOT/lib/release_tar.py" verify --archive "$OUT" "${VERIFY_EXEC_ARGS[@]}"      --sums "$STAGE/SHA256SUMS" --json "$OUT.verify.json" >/dev/null; then
  die "包后校验失败（归档成员模式/SHA256SUMS）: $OUT（见 $OUT.verify.json）" 3
fi
log "包后校验 PASS：归档 exec 位（${#EXEC_PATTERNS[@]} 组 glob）+ SHA256SUMS $FILE_COUNT 项"

# ---- 5 Linux 解包再校验（sha256sum -c + test -x；Windows 只做归档模式断言） ----------
EXTRACT_CHECK=0
[ "$(uname -s 2>/dev/null || printf unknown)" = "Linux" ] && EXTRACT_CHECK=1
[ "${TSPI_PKG_EXTRACT_CHECK:-0}" = "1" ] && EXTRACT_CHECK=1
if [ "$EXTRACT_CHECK" = "1" ]; then
  XDIR="$STAGE.extract"
  rm -rf "$XDIR"; mkdir -p "$XDIR"
  tar -xzf "$OUT" -C "$XDIR" || die "解包校验失败: $OUT" 3
  if ! ( cd "$XDIR" && sha256sum -c SHA256SUMS >/dev/null ); then
    die "解包后 SHA256SUMS 校验失败: $OUT" 3
  fi
  miss=0
  for pat in "${EXEC_PATTERNS[@]}"; do
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      if [ ! -x "$f" ]; then warn "[FAIL] 解包后无执行位: $f"; miss=$((miss+1)); fi
    done < <(cd "$XDIR" && compgen -G "$pat" 2>/dev/null | sed "s#^#$XDIR/#")
  done
  [ "$miss" -eq 0 ] || die "解包后执行位校验失败（$miss 个文件）: $OUT" 3
  rm -rf "$XDIR"
  log "解包校验 PASS：sha256sum -c（$FILE_COUNT 项）+ test -x（${#EXEC_PATTERNS[@]} 组 glob）"
else
  log "解包 exec 校验：SKIP（非 Linux 宿主；归档成员模式已由 release_tar.py verify 断言）"
fi

log "上传包完成: $OUT（release_id=$REL_ID files=$FILE_COUNT size=$(file_size_of "$OUT") sha256=$(sha256_of "$OUT")）"
log "校验入口: py $DEPLOY_ROOT/lib/release_tar.py verify --archive $OUT ${VERIFY_EXEC_ARGS[*]} --sums <解包后>/SHA256SUMS"
exit 0
