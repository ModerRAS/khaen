#!/usr/bin/env bash
# build-inferno.sh — Inferno（Dante/AoIP）+ ALSA 插件 + Statime（PTPv1 fork）
#
# 固定版本（2026-11-15 核对）：
#   Inferno  dev  9767558e0905c9e9c960f5a60e050fbdc38300b7  （GitLab lumifaza/inferno / GitHub teodly 镜像）
#   Statime  inferno-dev 244f20a56c173b1881f2e5e83652bb8b8209b2ab  （teodly/statime fork）
#
# 产物：
#   release/bin/inferno2pipe                         （可选录音工具）
#   release/bin/statime                              （PTPv1 follower daemon）
#   release/lib/libasound_module_pcm_inferno*.so     （ALSA 插件，装到 /usr/lib/aarch64-linux-gnu/alsa-lib/）
#
# 用法：./build-inferno.sh [--method native|cross] [--inferno-ref <sha/branch>] [--statime-ref <sha/branch>]
#                          [--gitlab|--github] [--offline]
# 依赖：cargo/rustup（>=1.67，建议 1.90+）、libasound2-dev、git；cross 方法需要 cross。
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/build-common.sh"

METHOD="native"
INFERNO_REF="9767558e0905c9e9c960f5a60e050fbdc38300b7"
STATIME_REF="244f20a56c173b1881f2e5e83652bb8b8209b2ab"
INFERNO_URL="https://github.com/teodly/inferno.git"
STATIME_URL="https://github.com/teodly/statime.git"
OFFLINE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --method) METHOD="$2"; shift 2 ;;
    --inferno-ref) INFERNO_REF="$2"; shift 2 ;;
    --statime-ref) STATIME_REF="$2"; shift 2 ;;
    --gitlab) INFERNO_URL="https://gitlab.com/lumifaza/inferno.git"; shift ;;
    --github) INFERNO_URL="https://github.com/teodly/inferno.git"; shift ;;
    --offline) OFFLINE=1; shift ;;
    -h|--help) sed -n "2,16p" "$0"; exit 0 ;;
    *) die "unknown option: $1" 2 ;;
  esac
done
need_cmd cargo
log "inferno: ref=$INFERNO_REF statime: ref=$STATIME_REF method=$METHOD"
record "inferno: ref=$INFERNO_REF statime_ref=$STATIME_REF method=$METHOD rustc=$(rustc -V 2>/dev/null || echo unknown)"

prepare_repo() { # url dir ref [fallback-branch]
  local url="$1" dir="$2" ref="$3" fb="${4:-}"
  if [[ -d "$dir/.git" ]]; then
    log "复用已有 checkout: $dir"
    if [[ "$OFFLINE" != 1 ]]; then
      if git -C "$dir" fetch --depth 1 origin "$ref" >&2; then
        git -C "$dir" checkout -f FETCH_HEAD >&2 || true
      elif [[ -n "$fb" ]] && git -C "$dir" fetch --depth 1 origin "$fb" >&2; then
        warn "按 sha 拉取失败，改用分支 $fb"; git -C "$dir" checkout -f FETCH_HEAD >&2 || true
      else
        warn "fetch 失败，沿用现有 checkout"
      fi
    fi
  else
    [[ "$OFFLINE" == 1 ]] && die "offline 模式需要预置源码: $dir" 3
    need_cmd git
    mkdir -p "$(dirname "$dir")"
    git clone --depth 1 --no-checkout "$url" "$dir" >&2
    if ! git -C "$dir" fetch --depth 1 origin "$ref" >&2; then
      [[ -n "$fb" ]] || die "git fetch 失败: $ref（无 fallback 分支）"
      warn "按 sha 拉取失败，改用分支 $fb"
      git -C "$dir" fetch --depth 1 origin "$fb" >&2 || die "git fetch 失败: $fb"
    fi
    git -C "$dir" checkout -f FETCH_HEAD >&2
  fi
}

INFERNO_SRC="$SRC_DIR/inferno"
STATIME_SRC="$SRC_DIR/statime"
prepare_repo "$INFERNO_URL" "$INFERNO_SRC" "$INFERNO_REF" "dev"
log "inferno HEAD: $(git -C "$INFERNO_SRC" rev-parse HEAD)"
if [[ "$OFFLINE" != 1 ]]; then
  git -C "$INFERNO_SRC" submodule update --init --recursive --depth 1 >&2 || die "inferno 子模块初始化失败（需要网络或 --offline 预置）"
else
  [[ -f "$INFERNO_SRC/usrvclock-rs/src/lib.rs" && -f "$INFERNO_SRC/searchfire/Cargo.toml" ]] \
    || die "offline: inferno 子模块未就绪（usrvclock-rs/searchfire）" 3
fi
prepare_repo "$STATIME_URL" "$STATIME_SRC" "$STATIME_REF" "inferno-dev"
log "statime HEAD: $(git -C "$STATIME_SRC" rev-parse HEAD)"

build_repo() {
  local dir="$1"
  if [[ "$METHOD" == "native" ]]; then
    ( cd "$dir" && cargo build --release --locked )
  else
    need_cmd cross
    ( cd "$dir" && cross build --release --locked --target aarch64-unknown-linux-gnu )
  fi
}

build_repo "$INFERNO_SRC"
build_repo "$STATIME_SRC"

if [[ "$METHOD" == "native" ]]; then
  INF_TARGET="$INFERNO_SRC/target/release"
  ST_TARGET="$STATIME_SRC/target/release"
else
  INF_TARGET="$INFERNO_SRC/target/aarch64-unknown-linux-gnu/release"
  ST_TARGET="$STATIME_SRC/target/aarch64-unknown-linux-gnu/release"
fi

[[ -x "$INF_TARGET/inferno2pipe" ]] || die "缺少 inferno2pipe: $INF_TARGET/inferno2pipe" 4
[[ -x "$ST_TARGET/statime" ]] || die "缺少 statime 二进制: $ST_TARGET/statime" 4
PLUGIN="$(ls -1 "$INF_TARGET"/libasound_module_pcm_inferno*.so 2>/dev/null | head -1 || true)"
[[ -n "$PLUGIN" ]] || die "缺少 ALSA 插件 .so（libasound_module_pcm_inferno*.so）" 4

install_artifact "$INF_TARGET/inferno2pipe" "$BIN_DIR/inferno2pipe" 755
install_artifact "$ST_TARGET/statime" "$BIN_DIR/statime" 755
install_artifact "$PLUGIN" "$LIB_DIR/$(basename "$PLUGIN")" 644
check_elf_aarch64 "$BIN_DIR/inferno2pipe"
check_elf_aarch64 "$BIN_DIR/statime"
check_elf_aarch64 "$LIB_DIR/$(basename "$PLUGIN")"
record "inferno: artifacts inferno2pipe/statime/$(basename "$PLUGIN")"
print_summary
