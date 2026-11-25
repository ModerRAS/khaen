#!/usr/bin/env bash
# khaen 发布安装脚本：校验 manifest -> 解包到 releases/<version> -> 原子切换 current -> 重启服务。
#
# 用法：sudo ./install.sh <发布的 tar.gz 路径> [--no-restart]
# 约定：/opt/khaen/releases/<version>/ 只读存放，/opt/khaen/current 是指向它的符号链接；
#       任何失败都在切换前中止，设备上不留半成品。
set -euo pipefail

ROOT="/opt/khaen"
RELEASES="${ROOT}/releases"
CURRENT="${ROOT}/current"
UNIT="khaen-daemon.service"

ARCHIVE="${1:-}"
RESTART=1
shift || true
for arg in "$@"; do
  case "${arg}" in
    --no-restart) RESTART=0 ;;
    *) echo "unknown argument: ${arg}" >&2; exit 2 ;;
  esac
done

if [[ -z "${ARCHIVE}" || ! -f "${ARCHIVE}" ]]; then
  echo "usage: $0 <khaen-<version>-linux-x86_64.tar.gz> [--no-restart]" >&2
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

echo "==> verify archive checksum (if provided)"
if [[ -f "${ARCHIVE}.sha256" ]]; then
  ( cd "$(dirname "${ARCHIVE}")" && sha256sum -c "$(basename "${ARCHIVE}.sha256")" )
fi

echo "==> unpack"
tar -xzf "${ARCHIVE}" -C "${WORK}"
STAGE="$(find "${WORK}" -maxdepth 1 -mindepth 1 -type d | head -n 1)"
[[ -n "${STAGE}" ]] || { echo "archive has no top-level directory" >&2; exit 1; }

echo "==> verify manifest"
python3 - "${STAGE}" <<'PY'
import hashlib, json, pathlib, sys
stage = pathlib.Path(sys.argv[1])
manifest = json.loads((stage / "manifest.json").read_text())
if manifest.get("schema") != "khaen-release/v1":
    sys.exit("unexpected manifest schema: %r" % manifest.get("schema"))
for entry in manifest["files"]:
    target = stage / entry["path"]
    if not target.is_file():
        sys.exit("missing file: %s" % entry["path"])
    digest = hashlib.sha256(target.read_bytes()).hexdigest()
    if digest != entry["sha256"]:
        sys.exit("checksum mismatch: %s" % entry["path"])
print("manifest ok: version=%s commit=%s files=%d" % (manifest["version"], manifest["commit"][:12], len(manifest["files"])))
PY

VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "${STAGE}/manifest.json")"
TARGET_DIR="${RELEASES}/${VERSION}"
if [[ -e "${TARGET_DIR}" ]]; then
  echo "release ${VERSION} already installed at ${TARGET_DIR}; refusing to overwrite" >&2
  exit 1
fi

echo "==> install to ${TARGET_DIR}"
install -d -m 0755 "${RELEASES}"
cp -a "${STAGE}" "${TARGET_DIR}"
chown -R root:root "${TARGET_DIR}"
chmod -R a-w "${TARGET_DIR}"

echo "==> switch current atomically"
ln -sfn "${TARGET_DIR}" "${CURRENT}.tmp"
mv -Tf "${CURRENT}.tmp" "${CURRENT}"

echo "==> install systemd unit"
if [[ -f "${TARGET_DIR}/deploy/${UNIT}" ]]; then
  install -m 0644 "${TARGET_DIR}/deploy/${UNIT}" "/etc/systemd/system/${UNIT}"
  systemctl daemon-reload
fi

if [[ "${RESTART}" -eq 1 ]]; then
  echo "==> restart ${UNIT}"
  systemctl enable --now "${UNIT}"
  systemctl restart "${UNIT}"
  systemctl --no-pager --lines=5 status "${UNIT}" || true
fi

echo "installed ${VERSION} -> ${CURRENT}"
echo "rollback: sudo $(dirname "$0")/rollback.sh"
