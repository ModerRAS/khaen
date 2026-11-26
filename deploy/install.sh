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
UNIT_RT="khaen-rt-host.service"
CONF_DIR="/etc/khaen"

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

echo "==> verify version consistency (package dir / manifest / BUILD)"
python3 - "${STAGE}" <<'PY'
import json, pathlib, re, sys
stage = pathlib.Path(sys.argv[1])
manifest = json.loads((stage / "manifest.json").read_text())
version = manifest["version"]
commit = manifest["commit"]
m = re.match(r"^khaen-(?P<version>.+)-linux-(?P<arch>x86_64|aarch64)$", stage.name)
if not m:
    sys.exit("unexpected package directory name: %s" % stage.name)
if m.group("version") != version:
    sys.exit("manifest version %r != package dir version %r" % (version, m.group("version")))
build = stage / "BUILD"
if not build.is_file():
    sys.exit("missing BUILD file (build provenance)")
fields = {}
for line in build.read_text().splitlines():
    if ":" in line:
        key, value = line.split(":", 1)
        fields[key.strip()] = value.strip()
if fields.get("version") != version:
    sys.exit("BUILD version %r != manifest version %r" % (fields.get("version"), version))
if fields.get("commit") != commit:
    sys.exit("BUILD commit %r != manifest commit %r" % (fields.get("commit"), commit))
if fields.get("target") != manifest.get("target"):
    sys.exit("BUILD target %r != manifest target %r" % (fields.get("target"), manifest.get("target")))
print("version consistency ok: %s (%s)" % (version, commit[:12]))
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

echo "==> install systemd units"
if [[ -f "${TARGET_DIR}/deploy/${UNIT}" ]]; then
  install -m 0644 "${TARGET_DIR}/deploy/${UNIT}" "/etc/systemd/system/${UNIT}"
  systemctl daemon-reload
fi
# 数据面 unit 只安装、不启用、不启动：出声需要窗口与明确许可（部署规则见工作区 AGENTS.md）。
if [[ -f "${TARGET_DIR}/deploy/${UNIT_RT}" ]]; then
  install -m 0644 "${TARGET_DIR}/deploy/${UNIT_RT}" "/etc/systemd/system/${UNIT_RT}"
  systemctl daemon-reload
fi

echo "==> install env examples (never overwrite existing)"
install -d -m 0755 "${CONF_DIR}"
for pair in "daemon.env.example:daemon.env" "rt-host.env.example:rt-host.env"; do
  example="${pair%%:*}"
  target="${pair##*:}"
  if [[ -f "${TARGET_DIR}/deploy/${example}" && ! -e "${CONF_DIR}/${target}" ]]; then
    install -m 0640 "${TARGET_DIR}/deploy/${example}" "${CONF_DIR}/${target}"
    echo "    installed ${CONF_DIR}/${target} from example; edit before starting"
  fi
done

if [[ "${RESTART}" -eq 1 ]]; then
  echo "==> restart ${UNIT}"
  systemctl enable --now "${UNIT}"
  systemctl restart "${UNIT}"
  systemctl --no-pager --lines=5 status "${UNIT}" || true
fi

if systemctl cat "${UNIT_RT}" >/dev/null 2>&1; then
  echo "note: ${UNIT_RT} installed but NOT enabled/started; it takes the ALSA device exclusively."
  echo "      start it only in an approved sound window: sudo systemctl start ${UNIT_RT}"
fi

echo "installed ${VERSION} -> ${CURRENT}"
echo "rollback: sudo $(dirname "$0")/rollback.sh"
