#!/usr/bin/env bash
# install-into-image.sh — 把音频栈（overlay + release + 离线 .deb）装进 Debian 12 rootfs
#
# 这是与 firmware-builder 的**集成契约**：在 rootfs 组装完成后、打包镜像前调用一次（可重复执行，幂等）。
# 全程**不联网**：软件包只从本地 --debs 目录安装；二进制只从 --release 复制。
#
# 用法：
#   sudo ./install-into-image.sh \
#     --rootfs  /path/to/debian12-arm64-rootfs \
#     --overlay ./overlay \
#     --release ./release \
#     --debs    ./release/debs \
#     --kernel  /path/to/kernel-6.1.build \
#     [--motu-ucm yes|no] [--enable-units yes|no] [--enable-boot-mode]
#     [--no-validate] [--no-exec] [--dry-run]
#
# 退出码：0 成功；1 用法错误；2 rootfs 不合规；3 输入文件缺失；4 dpkg 安装失败；5 校验失败。
set -uo pipefail

ROOTFS=""
OVERLAY="$(cd "$(dirname "$0")" && pwd)/overlay"
RELEASE="$(cd "$(dirname "$0")" && pwd)/release"
DEBS=""
KERNEL_DIR=""
KVER=""
MOTU_UCM="yes"
ENABLE_UNITS="yes"
ENABLE_BOOT_MODE=0
NO_VALIDATE=0
NO_EXEC=0
DRY=0

usage() { sed -n "2,18p" "$0" | sed "s/^# \{0,1\}//" >&2; exit "${1:-0}"; }
log()  { printf "[install] %s\n" "$*" >&2; }
warn() { printf "[install][warn] %s\n" "$*" >&2; }
die()  { printf "[install][error] %s\n" "$*" >&2; exit "${2:-1}"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rootfs) ROOTFS="$2"; shift 2 ;;
    --overlay) OVERLAY="$2"; shift 2 ;;
    --release) RELEASE="$2"; shift 2 ;;
    --debs) DEBS="$2"; shift 2 ;;
    --kernel) KERNEL_DIR="$2"; shift 2 ;;
    --kver) KVER="$2"; shift 2 ;;
    --motu-ucm) MOTU_UCM="$2"; shift 2 ;;
    --enable-units) ENABLE_UNITS="$2"; shift 2 ;;
    --enable-boot-mode) ENABLE_BOOT_MODE=1; shift ;;
    --no-validate) NO_VALIDATE=1; shift ;;
    --no-exec) NO_EXEC=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown option: $1" >&2; usage 1 ;;
  esac
done

[[ -n "$ROOTFS" ]] || usage 1
[[ -d "$ROOTFS" ]] || die "rootfs 不存在: $ROOTFS" 2
[[ -d "$OVERLAY" ]] || die "overlay 不存在: $OVERLAY" 3
[[ -d "$RELEASE" ]] || die "release 不存在: $RELEASE（先跑 build-all.sh）" 3
[[ -n "$DEBS" && -d "$DEBS" ]] || die "--debs 目录不存在（先跑 fetch-debs.sh）" 3
[[ "$DRY" == 0 && "$(id -u)" != "0" ]] && die "需要 root（dpkg/chroot/chown）" 1

log "rootfs=$ROOTFS overlay=$OVERLAY release=$RELEASE debs=$DEBS"

# --- 1. rootfs 合规检查 -------------------------------------------------------
[[ -f "$ROOTFS/etc/debian_version" ]] || die "不是 Debian rootfs（缺 /etc/debian_version）" 2
DEB_VER="$(cat "$ROOTFS/etc/debian_version" 2>/dev/null || true)"
case "$DEB_VER" in
  12.*) log "Debian version: $DEB_VER" ;;
  *) warn "Debian 版本为 $DEB_VER（目标是 12/bookworm）；继续但需自行确认 ABI" ;;
esac
ARCH="$(chroot "$ROOTFS" dpkg --print-architecture 2>/dev/null || true)"
[[ "$ARCH" == "arm64" ]] || warn "rootfs 架构=$ARCH（期望 arm64）；若为交叉构建请注意 qemu binfmt"

# chroot 执行能力：宿主 aarch64 或装了 qemu-user binfmt 才能跑 dpkg/chroot 校验
CAN_CHROOT=0
if [[ "$(uname -m)" == "aarch64" ]]; then CAN_CHROOT=1
elif [[ -e /proc/sys/fs/binfmt_misc/qemu-aarch64 ]]; then CAN_CHROOT=1; fi
if [[ "$CAN_CHROOT" == 0 && "$DRY" == 0 ]]; then
  if [[ "$NO_EXEC" == 1 ]]; then
    warn "宿主非 aarch64 且无 qemu binfmt：--no-exec 模式，将跳过 dpkg 安装与 chroot 校验（需自行确保包已装）"
  else
    die "宿主非 aarch64 且无 qemu-aarch64 binfmt：无法在 arm64 rootfs 内 dpkg。请安装 qemu-user-static+binfmt，或改用 --no-exec（自行装包），或在 aarch64 机器上执行" 2
  fi
fi

# --- 2. 准备 policy-rc.d（阻止 dpkg 安装时启动服务）---------------------------
POLICY="$ROOTFS/usr/sbin/policy-rc.d"
POLICY_CREATED=0
if [[ "$DRY" == 0 && ! -e "$POLICY" ]]; then
  printf "#!/bin/sh\nexit 101\n" > "$POLICY"; chmod 755 "$POLICY"; POLICY_CREATED=1
  log "临时创建 policy-rc.d（安装后删除）"
fi
cleanup() {
  if [[ "$POLICY_CREATED" == 1 && -e "$POLICY" ]]; then rm -f "$POLICY"; fi
  rm -rf "$ROOTFS/tmp/tspi-debs" 2>/dev/null || true
}
trap cleanup EXIT

# --- 3. 安装离线 .deb ---------------------------------------------------------
if [[ "$NO_EXEC" == 1 && "$DRY" == 0 ]]; then
  warn "--no-exec：跳过 dpkg 安装；请确保 release/debs 的包已由其它步骤装进 rootfs"
fi
if [[ "$NO_EXEC" == 0 ]] && compgen -G "$DEBS/*.deb" >/dev/null; then
  if [[ "$DRY" == 1 ]]; then
    log "DRY: 将安装 $(ls -1 "$DEBS"/*.deb | wc -l) 个 .deb"
  else
    mkdir -p "$ROOTFS/tmp/tspi-debs"
    cp -f "$DEBS"/*.deb "$ROOTFS/tmp/tspi-debs/"
    log "dpkg -i（chroot）…"
    chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive dpkg -i /tmp/tspi-debs/*.deb >/tmp/tspi-dpkg.log 2>&1 \
      || warn "dpkg -i 返回非 0（见 /tmp/tspi-dpkg.log）；尝试 dpkg --configure -a"
    chroot "$ROOTFS" env DEBIAN_FRONTEND=noninteractive dpkg --configure -a >/dev/null 2>&1 || true
    if chroot "$ROOTFS" dpkg --audit 2>/dev/null | grep -q .; then
      chroot "$ROOTFS" dpkg --audit >&2 || true
      die "dpkg 安装后仍有未配置包（不允许联网补齐）；检查 release/debs 完整性" 4
    fi
    log "已安装 $(ls -1 "$ROOTFS/tmp/tspi-debs"/*.deb | wc -l) 个包"
  fi
else
  [[ "$NO_EXEC" == 0 ]] && warn "--debs 目录没有 .deb：跳过包安装（仅适合已有完整依赖的 rootfs）"
fi

# --- 4. 复制 overlay ----------------------------------------------------------
if [[ "$DRY" == 1 ]]; then
  log "DRY: cp -a $OVERLAY/. -> $ROOTFS/"
else
  if command -v rsync >/dev/null 2>&1; then
    rsync -a "$OVERLAY/" "$ROOTFS/"
  else
    cp -a "$OVERLAY/." "$ROOTFS/"
  fi
  chown -R 0:0 "$ROOTFS/usr/local" "$ROOTFS/etc/systemd/system" "$ROOTFS/etc/default" "$ROOTFS/etc/security" "$ROOTFS/etc/modprobe.d" "$ROOTFS/etc/udev" "$ROOTFS/etc/camilladsp" "$ROOTFS/etc/statime" "$ROOTFS/etc/aes67-daemon" "$ROOTFS/etc/asound.conf" "$ROOTFS/etc/ptp4l-master-aes67.conf" "$ROOTFS/etc/tmpfiles.d" "$ROOTFS/usr/share/alsa/ucm2/USB-Audio/MOTU" 2>/dev/null || true
  # 强制修正执行位/权限（Windows 拷贝/共享目录会丢执行位；见 overlay-manifest.md）
  # 1) 所有 tspi-* 入口脚本；2) /usr/local/lib/tspi 下全部脚本（lib.sh/ucm-backport-mk5.sh/ptp-status-hook/phc-usrvclock）
  find "$ROOTFS/usr/local/bin" -maxdepth 1 -type f -name 'tspi-*' -exec chmod 755 {} + 2>/dev/null || true
  find "$ROOTFS/usr/local/lib/tspi" -maxdepth 1 -type f -exec chmod 755 {} + 2>/dev/null || true
  # 3) 配置/unit/udev/limits/modprobe/UCM 文本文件统一 644
  for d in "$ROOTFS/etc/systemd/system" "$ROOTFS/etc/default" "$ROOTFS/etc/security/limits.d" \
           "$ROOTFS/etc/modprobe.d" "$ROOTFS/etc/udev/rules.d" "$ROOTFS/etc/camilladsp" \
           "$ROOTFS/etc/statime" "$ROOTFS/etc/aes67-daemon" "$ROOTFS/etc/tmpfiles.d" \
           "$ROOTFS/usr/share/alsa/ucm2/USB-Audio/MOTU" ; do
    [ -d "$d" ] && find "$d" -maxdepth 1 -type f -exec chmod 644 {} + 2>/dev/null || true
  done
  chmod 644 "$ROOTFS/etc/asound.conf" "$ROOTFS/etc/ptp4l-master-aes67.conf" 2>/dev/null || true
  log "overlay 已复制"
fi

# --- 5. 复制 release 二进制 ---------------------------------------------------
copy_artifact() {
  local src="$1" dst="$2" mode="${3:-755}"
  [[ -f "$src" ]] || { warn "缺少产物: $src"; return 1; }
  if [[ "$DRY" == 1 ]]; then echo "DRY: cp $src -> $ROOTFS/$dst"; return 0; fi
  install -D -m "$mode" -o 0 -g 0 "$src" "$ROOTFS/$dst"
  log "  + $dst"
}

log "复制 CamillaDSP/Inferno/statime/aes67-daemon …"
MISSING=0
copy_artifact "$RELEASE/bin/camilladsp" "usr/local/bin/camilladsp" 755 || MISSING=$((MISSING+1))
copy_artifact "$RELEASE/bin/statime" "usr/local/bin/statime" 755 || MISSING=$((MISSING+1))
copy_artifact "$RELEASE/bin/inferno2pipe" "usr/local/bin/inferno2pipe" 755 || MISSING=$((MISSING+1))
copy_artifact "$RELEASE/bin/aes67-daemon" "usr/local/bin/aes67-daemon" 755 || MISSING=$((MISSING+1))
if [[ "$MISSING" -gt 0 ]]; then
  [[ "$NO_VALIDATE" == 1 ]] && warn "$MISSING 个产物缺失（--no-validate 继续）" || die "$MISSING 个必需产物缺失；先 build-all.sh" 3
fi

# Inferno ALSA 插件（tspi-mode dante 必需；release/lib/*.so* -> 系统 alsa-lib）
ALSA_LIB_DIR="usr/lib/aarch64-linux-gnu/alsa-lib"
PLUGIN_MISSING=0
if compgen -G "$RELEASE/lib/*.so*" >/dev/null; then
  for so in "$RELEASE"/lib/*.so*; do
    [[ -f "$so" ]] || continue
    copy_artifact "$so" "$ALSA_LIB_DIR/$(basename "$so")" 644 || PLUGIN_MISSING=1
  done
else
  PLUGIN_MISSING=1
fi
if [[ "$PLUGIN_MISSING" == 1 ]]; then
  if [[ "$NO_VALIDATE" == 1 ]]; then warn "缺少 Inferno ALSA 插件（release/lib/*.so*）；dante 模式不可用（--no-validate 继续）"
  else die "缺少 Inferno ALSA 插件（release/lib/libasound_module_pcm_inferno.so）；dante 模式不可用" 3; fi
fi

# 内核模块（可选但 AES67 模式必需）
if [[ -z "$KVER" && -n "$KERNEL_DIR" ]]; then
  KVER="$(cat "$KERNEL_DIR/include/config/kernel.release" 2>/dev/null || true)"
fi
if [[ -n "$KVER" ]]; then
  if [[ -f "$RELEASE/modules/MergingRavennaALSA.ko" ]]; then
    if [[ "$DRY" == 1 ]]; then echo "DRY: install LKM -> /lib/modules/$KVER/extra/"; else
      install -D -m 644 -o 0 -g 0 "$RELEASE/modules/MergingRavennaALSA.ko" "$ROOTFS/lib/modules/$KVER/extra/MergingRavennaALSA.ko"
      chroot "$ROOTFS" depmod -a "$KVER" >/dev/null 2>&1 || warn "depmod 失败（chroot 内 kmod 不可用？）"
      log "LKM 已装到 /lib/modules/$KVER/extra/"
    fi
  else
    warn "release/modules 无 MergingRavennaALSA.ko：AES67 模式将不可用"
  fi
else
  warn "未提供 --kernel/--kver：跳过内核模块安装"
fi

# --- 6. MOTU UCM 回移（可选） -------------------------------------------------
if [[ "$MOTU_UCM" == "yes" ]]; then
  if [[ -x "$OVERLAY/usr/local/lib/tspi/ucm-backport-mk5.sh" || -f "$OVERLAY/usr/local/lib/tspi/ucm-backport-mk5.sh" ]]; then
    if [[ "$DRY" == 1 ]]; then
      echo "DRY: ucm-backport-mk5.sh --rootfs $ROOTFS"
    else
      bash "$OVERLAY/usr/local/lib/tspi/ucm-backport-mk5.sh" --rootfs "$ROOTFS" \
        && log "MOTU mk5 UCM 回移完成" \
        || warn "MOTU mk5 UCM 回移失败（可选功能；主音频链路不依赖 UCM）"
    fi
  else
    warn "缺少 ucm-backport-mk5.sh（overlay 未复制？）"
  fi
fi

# --- 7. systemd enable（enabled 但 inactive：靠 ConditionPathExists 守卫）------
enable_unit() {
  local unit="$1" wants="$ROOTFS/etc/systemd/system/multi-user.target.wants"
  if [[ "$DRY" == 1 ]]; then echo "DRY: ln -s /etc/systemd/system/$unit $wants/$unit"; return 0; fi
  mkdir -p "$wants"
  ln -sfn "/etc/systemd/system/$unit" "$wants/$unit"
}

if [[ "$ENABLE_UNITS" == "yes" ]]; then
  log "enable systemd units（开机条件不满足 => 不启动；由 tspi-mode 控制）"
  for u in tspi-audio.target tspi-statime-inferno.service tspi-aes67-daemon.service \
           tspi-ravenna-lkm.service tspi-camilladsp.service tspi-ptp4l-master.service tspi-phc2sys.service \
           tspi-phc-usrvclock.service; do
    enable_unit "$u"
  done
  [[ "$ENABLE_BOOT_MODE" == 1 ]] && enable_unit tspi-boot-mode.service
else
  log "跳过 enable（--enable-units no）"
fi
mkdir -p "$ROOTFS/etc/tspi" 2>/dev/null || true

# --- 8. 生成清单 --------------------------------------------------------------
if [[ "$DRY" == 0 ]]; then
  {
    echo "# tspi-audio 安装清单（install-into-image.sh 生成）"
    echo "# date: $(date -Is)"
    echo "# rootfs debian: $DEB_VER arch: $ARCH kver: ${KVER:-unknown}"
    echo
    echo "## overlay 文件"
    ( cd "$OVERLAY" && find . -type f -o -type l | sort | sed "s#^\./##" )
    echo
    echo "## release 产物 sha256"
    [[ -f "$RELEASE/MANIFEST.sha256" ]] && sed "s/^/  /" "$RELEASE/MANIFEST.sha256" || echo "  (无 MANIFEST.sha256)"
    echo
    echo "## 已装包（版本）"
    chroot "$ROOTFS" dpkg-query -W -f '${Package} ${Version}\n' 2>/dev/null \
      | grep -E '^(alsa-utils|alsa-ucm-conf|libasound2|linuxptp|avahi-daemon|libavahi|libboost|ethtool|jq|netcat-openbsd|socat) ' \
      | sed "s/^/  /" || true
  } > "$ROOTFS/etc/tspi-audio-manifest.txt" 2>/dev/null || warn "生成 manifest 失败"
  log "清单: /etc/tspi-audio-manifest.txt"
fi

# --- 9. 校验 ------------------------------------------------------------------
if [[ "$NO_VALIDATE" == 1 || "$DRY" == 1 ]]; then
  log "跳过校验（--no-validate/--dry-run）"
  exit 0
fi

FAIL=0
validate() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then log "  OK   $desc"; else warn "  FAIL $desc"; FAIL=$((FAIL+1)); fi
}

log "校验 …"
for f in etc/systemd/system/tspi-audio.target \
         etc/systemd/system/tspi-camilladsp.service \
         etc/systemd/system/tspi-ptp4l-master.service \
         etc/systemd/system/tspi-phc2sys.service \
         etc/systemd/system/tspi-phc-usrvclock.service \
         etc/statime/inferno-ptpv1.toml \
         etc/ptp4l-master-aes67.conf \
         etc/aes67-daemon/daemon.conf \
         etc/asound.conf \
         etc/camilladsp/dante.yml etc/camilladsp/aes67.yml \
         usr/local/bin/tspi-mode usr/local/bin/tspi-ptp-status \
         usr/local/lib/tspi/lib.sh usr/local/lib/tspi/ptp-status-hook \
         usr/local/lib/tspi/phc-usrvclock ; do
  [[ -e "$ROOTFS/$f" ]] && log "  OK   $f" || { warn "  MISS $f"; FAIL=$((FAIL+1)); }
done
[[ -x "$ROOTFS/usr/local/bin/camilladsp" ]] && log "  OK   camilladsp" || { warn "  MISS camilladsp"; FAIL=$((FAIL+1)); }

if [[ "$NO_EXEC" == 0 ]]; then
  if [[ "$(uname -m)" == "aarch64" || -e /proc/sys/fs/binfmt_misc/qemu-aarch64 ]]; then
    validate "camilladsp --version" chroot "$ROOTFS" /usr/local/bin/camilladsp --version
    validate "statime --version" chroot "$ROOTFS" /usr/local/bin/statime --version
    validate "ptp4l -v" chroot "$ROOTFS" /usr/sbin/ptp4l -v
    validate "systemd units" chroot "$ROOTFS" systemd-analyze verify /etc/systemd/system/tspi-audio.target
  else
    warn "宿主非 aarch64 且无 qemu-aarch64：跳过 chroot 执行校验"
  fi
fi

if [[ "$FAIL" -gt 0 ]]; then
  die "校验失败 $FAIL 项（见上）" 5
fi
log "完成。镜像内默认状态：off（服务 enabled 但被 ConditionPathExists 守卫）；首次使用执行 tspi-mode dante|aes67|b2"
exit 0
