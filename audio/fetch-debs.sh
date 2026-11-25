#!/usr/bin/env bash
# fetch-debs.sh — 为 Debian 12 (bookworm) aarch64 目标离线抓齐 .deb 包池
#
# 在任意 Linux 主机执行（只需 curl/gzip/sha256sum/awk，不需要 apt，也不需要 aarch64 环境）。
# 步骤：读 packages.list 的 [runtime] 段（--with-test 追加 [test] 段）→ 下载 bookworm main
#       的 binary-arm64 Packages 索引 → 取每个包的版本/Filename/SHA256 → 下载并校验 → 写 MANIFEST.txt。
#
# 产物：release/debs/*.deb + MANIFEST.txt，供 install-into-image.sh --debs 离线安装。
#
# 用法：
#   ./fetch-debs.sh [--out release/debs] [--mirror http://deb.debian.org/debian]
#                   [--suite bookworm] [--arch arm64] [--packages packages.list] [--with-test]
# 退出码：0 成功；1 用法错误；2 下载/校验失败。
set -euo pipefail

OUT="release/debs"
MIRROR="http://deb.debian.org/debian"
SUITE="bookworm"
ARCH="arm64"
WITH_TEST=0
PKGFILE="packages.list"

usage() { sed -n "2,15p" "$0" | sed "s/^# \{0,1\}//"; exit "${1:-0}"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --mirror) MIRROR="$2"; shift 2 ;;
    --suite) SUITE="$2"; shift 2 ;;
    --arch) ARCH="$2"; shift 2 ;;
    --packages) PKGFILE="$2"; shift 2 ;;
    --with-test) WITH_TEST=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown option: $1" >&2; usage 1 ;;
  esac
done

[[ -f "$PKGFILE" ]] || { echo "ERROR: packages file not found: $PKGFILE" >&2; exit 1; }
command -v curl >/dev/null || { echo "ERROR: curl required" >&2; exit 2; }
mkdir -p "$OUT"

list_pkgs() {
  awk -v with_test="$WITH_TEST" '
    /^## ===== \[runtime\]/ { sec="runtime"; next }
    /^## ===== \[test\]/    { sec="test"; next }
    /^## ===== \[/         { sec="other"; next }
    /^[a-z0-9][a-z0-9+.-]*[ \t]*\|/ {
      if (sec == "runtime" || (sec == "test" && with_test == 1)) {
        split($0, a, "|"); gsub(/[ \t]+/, "", a[1]); print a[1]
      }
    }' "$PKGFILE" | sort -u
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "== fetch-debs: suite=$SUITE arch=$ARCH mirror=$MIRROR out=$OUT"
IDX="$MIRROR/dists/$SUITE/main/binary-$ARCH/Packages.gz"
echo "fetching index: $IDX"
curl -fsSL --retry 3 -o "$TMP/Packages.gz" "$IDX"
gzip -dc "$TMP/Packages.gz" > "$TMP/Packages"
echo "index paragraphs: $(grep -c '^Package: ' "$TMP/Packages")"

# 把每个包的最新元数据做成 name -> "version|filename|sha256" 的查询表
awk '
  BEGIN { RS=""; FS="\n" }
  {
    name=""; ver=""; fn=""; sha="";
    for (i = 1; i <= NF; i++) {
      if ($i ~ /^Package: /) name=substr($i, 10);
      if ($i ~ /^Version: /) ver=substr($i, 10);
      if ($i ~ /^Filename: /) fn=substr($i, 11);
      if ($i ~ /^SHA256: /) sha=substr($i, 9);
    }
    if (name != "") print name "\t" ver "\t" fn "\t" sha;
  }' "$TMP/Packages" > "$TMP/index.tsv"

declare -A META
while IFS=$'\t' read -r n v fn sha; do META["$n"]="$v|$fn|$sha"; done < "$TMP/index.tsv"

MISSING=0; COUNT=0
: > "$OUT/MANIFEST.txt"
while read -r pkg; do
  meta="${META[$pkg]:-}"
  if [[ -z "$meta" ]]; then
    echo "WARN: not found in $SUITE/main ($ARCH): $pkg" >&2
    MISSING=$((MISSING + 1))
    continue
  fi
  ver="${meta%%|*}"; rest="${meta#*|}"; fn="${rest%%|*}"; sha="${rest##*|}"
  base="$(basename "$fn")"
  if [[ -f "$OUT/$base" ]]; then
    echo "exists:   $base ($ver)"
  else
    echo "download: $base ($ver)"
    curl -fsSL --retry 3 -o "$OUT/$base.part" "$MIRROR/$fn"
    mv "$OUT/$base.part" "$OUT/$base"
  fi
  actual="$(sha256sum "$OUT/$base" | awk '{print $1}')"
  if [[ -n "$sha" && "$actual" != "$sha" ]]; then
    echo "ERROR: sha256 mismatch for $base" >&2
    echo "  expected: $sha" >&2
    echo "  actual:   $actual" >&2
    exit 2
  fi
  printf "%s %s %s %s\n" "$pkg" "$ver" "$base" "$actual" >> "$OUT/MANIFEST.txt"
  COUNT=$((COUNT + 1))
done < <(list_pkgs)

echo "== done: $COUNT package(s), missing=$MISSING"
echo "   debs: $OUT"
echo "   manifest: $OUT/MANIFEST.txt"
if [[ "$MISSING" -gt 0 ]]; then
  echo "WARN: $MISSING package(s) missing — 检查 packages.list 的 [nonfree/B2] 段或换 --suite bookworm-backports" >&2
fi
exit 0
