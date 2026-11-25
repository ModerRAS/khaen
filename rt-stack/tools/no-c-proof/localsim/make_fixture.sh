#!/usr/bin/env bash
# T72 no-C 本地夹具生成器（只读脚本的本地干跑；不连板、不触碰真实 systemd/设备）
#
# 生成一个可被 board_check_no_c.sh --root <dir> 扫描的假板根：
#   board/proc/{pid}/{comm,cmdline,cgroup,exe,fd}、board/proc/asound、board/dev/snd、
#   board/etc/systemd/system、board/run、board/opt/khaen-rt-stack、board/sys/fs/cgroup
# 另生成 mock 状态：<out>/mock/{units,active,enabled,ss.txt}，由 rt-stack/deploy/tests/mocks 的
# systemctl/ss 提供（PATH 前置）；绝不触碰真实系统。
#
# 用法: ./make_fixture.sh <variant> <out-dir>
#   variant: pass | pass-rollback-mute | fail-proc | fail-unit | fail-cgroup | fail-port1234 |
#            fail-port18080 | fail-entry | fail-socketfd | fail-pcm-owner | fail-daemon-pcm |
#            fail-keeper-before | fail-wants | fail-duplicate-host
set -uo pipefail
# Windows Git Bash: 让 ln -s 生成真实符号链接（否则退化为复制，fixture 的 /proc/*/exe、fd 无法被 readlink 识别）
export MSYS=winsymlinks:native
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS="$(cd "$HERE/../../.." && pwd)"          # rt-stack
PROJECT="$(cd "$RS/.." && pwd)"             # projects/khaen-audio-dsp
VARIANT="${1:?usage: make_fixture.sh <variant> <out-dir>}"
OUT="${2:?usage: make_fixture.sh <variant> <out-dir>}"
rm -rf "$OUT"
B="$(cygpath -u "$OUT/board" 2>/dev/null || printf '%s' "$OUT/board")"
M="$(cygpath -u "$OUT/mock" 2>/dev/null || printf '%s' "$OUT/mock")"
mkdir -p "$B"/proc/asound/card4/pcm0p/sub0 "$B"/proc/sys/kernel/random \
         "$B"/dev/snd "$B"/etc/systemd/system/multi-user.target.wants "$B"/etc/khaen-rt-stack \
         "$B"/opt/khaen-rt-stack/current/bin "$B"/run "$B"/run/khaen-rt-stack \
         "$B"/sys/fs/cgroup/system.slice "$B"/usr/local/bin \
         "$M"/units "$M"/active "$M"/enabled

# ---- mock systemd 单元仓库 ---------------------------------------------------
cp "$RS"/service/khaen-rt-host.service "$RS"/service/khaen-rt-daemon.service "$RS"/service/khaen-rt-stack.target "$M"/units/ 2>/dev/null || true
grep -v '^Before=' "$RS"/deploy/config/khaen-motu-48k-keeper.service > "$M"/units/khaen-motu-48k-keeper.service 2>/dev/null || true
for u in khaen-camilladsp.service khaen-upmix-autodetect.service khaen-audio.target khaen-boot-mode.service khaen-audio-web.service; do
  printf '[Unit]\nDescription=old C/old entry stub (%s) - masked+inactive fixture\n' "$u" > "$M"/units/"$u"
  printf 'masked\n' > "$M"/enabled/"$u"
done
printf 'enabled\n' > "$M"/enabled/khaen-rt-stack.target
printf 'enabled\n' > "$M"/enabled/khaen-motu-48k-keeper.service
touch "$M"/active/khaen-rt-host.service "$M"/active/khaen-rt-daemon.service "$M"/active/khaen-rt-stack.target
: > "$M"/ss.txt
: > "$M"/procs

# ---- 假板文件系统 -----------------------------------------------------------
printf ' 3 [Umik1         ]: USB-Audio - Umik-1 Gain:18dB\n 4 [UltraLitemk5   ]: USB-Audio - UltraLite mk5 at usb-0000:00:14.0-1, high speed\n' > "$B"/proc/asound/cards
printf 'state: RUNNING\nowner_pid   : 4321\n' > "$B"/proc/asound/card4/pcm0p/sub0/status
printf '1234567.89 2345678.90\n' > "$B"/proc/uptime
printf 'fixture-boot-id-0001\n' > "$B"/proc/sys/kernel/random/boot_id
: > "$B"/dev/snd/pcmC4D0p; : > "$B"/dev/snd/pcmC4D0c; : > "$B"/dev/snd/controlC4; : > "$B"/dev/snd/pcmC3D0c
printf '#!/bin/sh\necho khaen-rt-host-fixture\n' > "$B"/opt/khaen-rt-stack/current/bin/khaen-rt-host
printf '#!/bin/sh\necho khaen-rt-daemon-fixture\n' > "$B"/opt/khaen-rt-stack/current/bin/khaen-rt-daemon
chmod +x "$B"/opt/khaen-rt-stack/current/bin/khaen-rt-host "$B"/opt/khaen-rt-stack/current/bin/khaen-rt-daemon 2>/dev/null || true
printf 'block_size = 128\n' > "$B"/etc/khaen-rt-stack/rt-host.toml
printf 'bind = "127.0.0.1:8090"\n' > "$B"/etc/khaen-rt-stack/rt-daemon.toml
ln -s /etc/systemd/system/khaen-rt-stack.target "$B"/etc/systemd/system/multi-user.target.wants/khaen-rt-stack.target 2>/dev/null || true

# 进程：1=init，4321=host（MOTU PCM owner），4322=daemon，4323=journald（其它声卡持有者）
mkproc() { # pid comm cmdline cgroup
  local pid="$1" comm="$2" cmdline="$3" cg="$4" d="$B/proc/$1"
  mkdir -p "$d/fd"
  printf '%s\n' "$comm" > "$d/comm"
  printf '%s\n' "$cmdline" > "$d/cmdline"
  printf '0::%s\n' "$cg" > "$d/cgroup"
  ln -s /dev/null "$d/fd/0" 2>/dev/null || true
  ln -s /dev/null "$d/fd/1" 2>/dev/null || true
}
mkproc 1 systemd "/sbin/init" "/init.scope"
mkproc 4321 khaen-rt-host "khaen-rt-host run --config /etc/khaen-rt-stack/rt-host.toml" "/system.slice/khaen-rt-host.service"
mkproc 4322 khaen-rt-daemon "khaen-rt-daemon serve --config /etc/khaen-rt-stack/rt-daemon.toml" "/system.slice/khaen-rt-daemon.service"
mkproc 4323 systemd-journal "systemd-journald" "/system.slice/systemd-journald.service"
ln -s "$B/opt/khaen-rt-stack/current/bin/khaen-rt-host" "$B/proc/4321/exe" 2>/dev/null || true
ln -s "$B/opt/khaen-rt-stack/current/bin/khaen-rt-daemon" "$B/proc/4322/exe" 2>/dev/null || true
ln -s "$B/dev/snd/pcmC4D0p" "$B/proc/4321/fd/5" 2>/dev/null || true
ln -s "$B/dev/snd/pcmC3D0c" "$B/proc/4323/fd/6" 2>/dev/null || true

# ---- 变体注入 --------------------------------------------------------------
case "$VARIANT" in
  pass) : ;;
  pass-rollback-mute)
    rm -rf "$B/proc/4321"
    printf 'state: CLOSED\n' > "$B/proc/asound/card4/pcm0p/sub0/status" ;;
  fail-proc)
    mkproc 4400 camilladsp "camilladsp -p 1234 -a 127.0.0.1 -l warn /run/khaen/camilladsp.yml" "/system.slice/khaen-camilladsp.service"
    : ;;
  fail-unit)
    touch "$M"/active/khaen-camilladsp.service
    printf 'enabled\n' > "$M"/enabled/khaen-camilladsp.service ;;
  fail-cgroup)
    mkproc 4401 khaen-upmix "khaen-upmix --presets /etc/khaen-audio/upmix/presets.yml autodetect" "/system.slice/khaen-upmix.service"
    printf '0::/system.slice/khaen-camilladsp.service\n' > "$B/proc/4401/cgroup" ;;
  fail-port1234)
    printf 'LISTEN 0 128 127.0.0.1:1234 0.0.0.0:* users:((\"camilladsp\",pid=4400,fd=3))\n' > "$M"/ss.txt ;;
  fail-port18080)
    printf 'LISTEN 0 128 0.0.0.0:18080 0.0.0.0:* users:((\"old-web\",pid=4400,fd=3))\n' > "$M"/ss.txt ;;
  fail-entry)
    mkdir -p "$B"/run/khaen "$B"/etc/khaen
    printf 'x\n' > "$B"/run/khaen/camilladsp.yml
    printf 'off\n' > "$B"/etc/khaen/last-mode ;;
  fail-socketfd)
    printf 'socket\n' > "$B"/run/camilladsp.sock
    ln -s "$B/run/camilladsp.sock" "$B/proc/4321/fd/8" 2>/dev/null || true ;;
  fail-pcm-owner)
    ln -s "$B/dev/snd/pcmC4D0p" "$B/proc/4323/fd/7" 2>/dev/null || true ;;
  fail-daemon-pcm)
    ln -s "$B/dev/snd/pcmC4D0p" "$B/proc/4322/fd/9" 2>/dev/null || true ;;
  fail-keeper-before)
    cp "$RS"/deploy/config/khaen-motu-48k-keeper.service "$M"/units/khaen-motu-48k-keeper.service ;;
  fail-wants)
    ln -sfn /dev/null "$B"/etc/systemd/system/multi-user.target.wants/khaen-camilladsp.service 2>/dev/null || true ;;
  fail-duplicate-host)
    mkproc 4402 khaen-rt-host "khaen-rt-host run --config /etc/khaen-rt-stack/rt-host.toml" "/system.slice/khaen-rt-host.service"
    ln -s "$B/opt/khaen-rt-stack/current/bin/khaen-rt-host" "$B/proc/4402/exe" 2>/dev/null || true ;;
  *) echo "unknown variant: $VARIANT" >&2; exit 2 ;;
esac
printf 'fixture variant=%s built at %s\n' "$VARIANT" "$OUT"
