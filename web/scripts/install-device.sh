#!/usr/bin/env bash
# 安装 khaen-audio-web 到设备或 OEM 镜像 rootfs。
#
# 用法：
#   sudo ./scripts/install-device.sh --binary /path/to/khaen-audio-web
#   sudo ./scripts/install-device.sh --binary ./target/aarch64-unknown-linux-gnu/release/khaen-audio-web
#   sudo ./scripts/install-device.sh --binary ... --rootfs /path/to/rootfs   # 不执行 systemctl
#   sudo ./scripts/install-device.sh --config-only                           # 只补配置/unit
#   ./scripts/install-device.sh --binary ... --rootfs /tmp/rootfs --dry-run  # 只打印动作，不落盘（交接验收用）
#
# 安装布局：
#   /usr/local/bin/khaen-audio-web                    二进制（静态资源已内嵌，无额外文件依赖）
#   /etc/khaen-audio-web/config.toml                  配置（0600；首次启动自动生成 token）
#   /etc/systemd/system/khaen-audio-web.service       systemd unit（enabled）
#   /etc/systemd/system/multi-user.target.wants/...  开机自启 symlink（--rootfs 时创建）
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
BIN=""
ROOTFS=""
CONFIG_ONLY=0
NO_START=0
DRY_RUN=0

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --binary) BIN="${2:?--binary 需要路径}"; shift 2 ;;
    --rootfs) ROOTFS="${2:?--rootfs 需要路径}"; shift 2 ;;
    --config-only) CONFIG_ONLY=1; shift ;;
    --no-start) NO_START=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

TARGET_ROOT="${ROOTFS:-/}"
p() { echo "${TARGET_ROOT%/}$1"; }

# dry-run：只打印将执行的动作，不做任何写操作（用于 OEM 交接/CI 验收）
run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] $*"
  else
    "$@"
  fi
}
say() { echo "==> $*"; }

if [ "$DRY_RUN" -eq 0 ] && [ "$(id -u)" -ne 0 ]; then
  echo "error: 需要 root（安装到 / 或 rootfs 目录）；只做检查请加 --dry-run" >&2
  exit 1
fi

if [ -n "$ROOTFS" ] && [ "$DRY_RUN" -eq 0 ] && [ ! -d "$ROOTFS" ]; then
  echo "error: rootfs 目录不存在: $ROOTFS" >&2
  exit 1
fi

if [ "$CONFIG_ONLY" -eq 0 ]; then
  if [ -z "$BIN" ]; then
    echo "error: 请用 --binary 指定 khaen-audio-web 可执行文件" >&2
    exit 1
  fi
  if [ ! -f "$BIN" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      echo "warn: [dry-run] 二进制不存在: $BIN（仅检查契约）" >&2
    else
      echo "error: 二进制不存在: $BIN" >&2
      exit 1
    fi
  fi
  run install -d -m 0755 "$(p /usr/local/bin)"
  run install -m 0755 "$BIN" "$(p /usr/local/bin/khaen-audio-web)"
  say "安装二进制: $(p /usr/local/bin/khaen-audio-web)"
fi

run install -d -m 0750 "$(p /etc/khaen-audio-web)"
if [ "$DRY_RUN" -eq 1 ] || [ ! -f "$(p /etc/khaen-audio-web/config.toml)" ]; then
  # 保留 token = "CHANGE_ME"：首次启动自动生成随机 token 并打印到 journald
  run install -m 0600 "$ROOT/deploy/config.toml.example" "$(p /etc/khaen-audio-web/config.toml)"
  say "写入默认配置: $(p /etc/khaen-audio-web/config.toml)（token 首次启动自动生成）"
else
  say "配置已存在，保持不动: $(p /etc/khaen-audio-web/config.toml)"
fi

run install -d -m 0755 "$(p /etc/systemd/system)"
run install -m 0644 "$ROOT/deploy/khaen-audio-web.service" "$(p /etc/systemd/system/khaen-audio-web.service)"
say "安装 systemd unit: $(p /etc/systemd/system/khaen-audio-web.service)"

if [ -n "$ROOTFS" ]; then
  # 镜像预装：不执行 systemctl，用 symlink 表达 enable
  run install -d -m 0755 "$(p /etc/systemd/system/multi-user.target.wants)"
  run ln -sfn /etc/systemd/system/khaen-audio-web.service \
    "$(p /etc/systemd/system/multi-user.target.wants/khaen-audio-web.service)"
  say "已在 rootfs 中 enable（multi-user.target.wants symlink）"
  say "首次开机启动后查看 token：journalctl -u khaen-audio-web -b | grep -i token"
elif [ "$DRY_RUN" -eq 1 ]; then
  say "[dry-run] systemctl daemon-reload && systemctl enable khaen-audio-web.service"
  if [ "$NO_START" -eq 0 ]; then
    say "[dry-run] systemctl restart khaen-audio-web.service"
  fi
else
  systemctl daemon-reload
  systemctl enable khaen-audio-web.service
  if [ "$NO_START" -eq 0 ]; then
    systemctl restart khaen-audio-web.service
    say "已启动 khaen-audio-web.service（监听 0.0.0.0:80，默认；如被占用请改 /etc/khaen-audio-web/config.toml 的 listen 回退 8088 后重启）"
    say "查看 token：journalctl -u khaen-audio-web -b | grep -i token"
  else
    say "已 enable（未启动；需要时 systemctl start khaen-audio-web）"
  fi
fi

echo
echo "完成。默认端口 80（浏览器直接 http://<设备IP>/；如被占用可改 listen 回退 8088）；首次启动会自动生成访问 token 并打印到 journald。"
echo "打印当前 token：/usr/local/bin/khaen-audio-web --config /etc/khaen-audio-web/config.toml --print-token"
