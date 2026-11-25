#!/usr/bin/env bash
# build-common.sh — 构建脚本公共函数（被 build-*.sh source）
# 约定：所有产物落 release/{bin,lib,modules,debs,src,build-info.txt}；不联网时自动改用 release/src 下的本地包。
set -euo pipefail

PROJ_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REL_DIR="${REL_DIR:-$PROJ_ROOT/release}"
BIN_DIR="${BIN_DIR:-$REL_DIR/bin}"
LIB_DIR="${LIB_DIR:-$REL_DIR/lib}"
MOD_DIR="${MOD_DIR:-$REL_DIR/modules}"
SRC_DIR="${SRC_DIR:-$REL_DIR/src}"
BUILD_INFO="$REL_DIR/build-info.txt"

log()  { printf "[build] %s\n" "$*" >&2; }
warn() { printf "[build][warn] %s\n" "$*" >&2; }
die()  { printf "[build][error] %s\n" "$*" >&2; exit "${2:-1}"; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "缺少命令: $1（见 packages.list [build]）"
}

ensure_dirs() { mkdir -p "$BIN_DIR" "$LIB_DIR" "$MOD_DIR" "$SRC_DIR" "$REL_DIR"; }

# 记录一条构建信息（追加到 release/build-info.txt）
record() {
  mkdir -p "$REL_DIR"
  printf "%s | %s\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$BUILD_INFO"
}

# 校验 ELF 目标架构（aarch64 用 readelf/file；在 x86 构建机上用 file 即可）
check_elf_aarch64() {
  local f="$1"
  [[ -f "$f" ]] || die "产物不存在: $f"
  if command -v file >/dev/null 2>&1; then
    local desc; desc="$(file -b "$f")"
    case "$desc" in
      *aarch64*|*ARM\ aarch64*) log "OK ELF aarch64: $(basename "$f")" ;;
      *ELF*) warn "ELF 但非 aarch64（如需上板请确认）: $f -> $desc" ;;
      *) warn "非 ELF: $f -> $desc" ;;
    esac
  fi
}

# 安装一个文件到目标目录并记录 sha256
install_artifact() {
  local src="$1" dst="$2" mode="${3:-755}"
  [[ -f "$src" ]] || die "source 不存在: $src"
  install -D -m "$mode" "$src" "$dst"
  sha256sum "$dst" | awk '{print $1}' > "$dst.sha256"
  log "installed: $dst ($(stat -c %s "$dst") bytes)"
}

# 计算文件的 sha256（无 sha256sum 时回退）
sha_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
  else openssl dgst -sha256 "$1" | awk '{print $NF}'; fi
}

# 下载（若本地已存在则跳过）；$2 = 目标路径
download() {
  local url="$1" dst="$2"
  if [[ -f "$dst" ]]; then log "已存在，跳过下载: $dst"; return 0; fi
  command -v curl >/dev/null 2>&1 || die "需要 curl 下载 $url（或手动放到 $dst）"
  log "下载: $url"
  curl -fL --retry 3 --continue-at - -o "$dst.part" "$url" || die "下载失败: $url"
  mv "$dst.part" "$dst"
}

# git 拉取/检出（浅克隆到 src 目录，可复用）
git_fetch_pinned() {
  local url="$1" dir="$2" ref="$3"
  if [[ ! -d "$dir/.git" ]]; then
    need_cmd git
    log "git clone: $url -> $dir"
    git clone --filter=blob:none --no-checkout "$url" "$dir" >&2 || die "git clone 失败: $url"
  fi
  git -C "$dir" fetch --depth 1 origin "$ref" >&2 || die "git fetch 失败: $ref"
  git -C "$dir" checkout -f FETCH_HEAD >&2 || die "git checkout 失败: $ref"
  git -C "$dir" rev-parse HEAD
}

print_summary() {
  log "================ 产物摘要 ================"
  find "$BIN_DIR" "$LIB_DIR" "$MOD_DIR" -maxdepth 1 -type f -printf "%-55p %10s bytes\n" 2>/dev/null | sort || true
  log "build-info: $BUILD_INFO"
}

ensure_dirs
