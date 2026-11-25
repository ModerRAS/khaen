#!/usr/bin/env bash
# khaen 回滚：把 current 指回上一个 release 并重启服务。
#
# 用法：sudo ./rollback.sh [<version>]
#   不带参数：回到 releases/ 中按名字排序的上一个版本。
#   带参数：切到指定版本（必须已安装）。
set -euo pipefail

ROOT="/opt/khaen"
RELEASES="${ROOT}/releases"
CURRENT="${ROOT}/current"
UNIT="khaen-daemon.service"

CURRENT_TARGET="$(readlink -f "${CURRENT}" || true)"
[[ -n "${CURRENT_TARGET}" ]] || { echo "current symlink is missing; nothing to roll back from" >&2; exit 1; }
CURRENT_VERSION="$(basename "${CURRENT_TARGET}")"

if [[ $# -ge 1 ]]; then
  TARGET_VERSION="$1"
else
  TARGET_VERSION="$(ls -1 "${RELEASES}" | grep -v -x "${CURRENT_VERSION}" | sort -V | tail -n 1)"
fi
[[ -n "${TARGET_VERSION}" ]] || { echo "no other release installed" >&2; exit 1; }

TARGET_DIR="${RELEASES}/${TARGET_VERSION}"
[[ -d "${TARGET_DIR}" ]] || { echo "release not installed: ${TARGET_VERSION}" >&2; exit 1; }

echo "rollback ${CURRENT_VERSION} -> ${TARGET_VERSION}"
ln -sfn "${TARGET_DIR}" "${CURRENT}.tmp"
mv -Tf "${CURRENT}.tmp" "${CURRENT}"
systemctl restart "${UNIT}"
systemctl --no-pager --lines=5 status "${UNIT}" || true
echo "current -> ${CURRENT}"
