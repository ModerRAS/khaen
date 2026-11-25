#!/bin/bash
# nsp7-wait-stream.sh —— NSP-first 启动闸门（khaen-rt-host-nsp7.service 的 ExecStart）
#
# 为什么需要：khaen-rt-host 的 userspace AES67 失流门限为 aes67_stall_blocks=400
# （128 帧块 -> 约 1.07 s），持续失流 2x 后 fail-closed 退出。若 host 先于 NSP 发送端
# 启动（host-first），必然 fail-closed。本脚本等 NSP 的 RTP（目的 udp/5004）真实出现后
# 才 exec host，保证 NSP-first；NSP 会话中断/重启后 systemd 重启本 unit，仍先等流。
#
# 环境变量（unit 注入）：
#   TSPI_HOST_CONFIG  默认 /etc/tspi-rt-stack/rt-host-nsp7.toml
#   TSPI_HOST_BIN     默认 /usr/local/lib/tspi-rt-stack/f510ed64/khaen-rt-host
#   TSPI_AES67_IFACE  默认 enp2s0
#   TSPI_AES67_PORT   默认 5004
set -u
IFACE="${TSPI_AES67_IFACE:-enp2s0}"
PORT="${TSPI_AES67_PORT:-5004}"
CFG="${TSPI_HOST_CONFIG:-/etc/tspi-rt-stack/rt-host-nsp7.toml}"
BIN="${TSPI_HOST_BIN:-/usr/local/lib/tspi-rt-stack/f510ed64/khaen-rt-host}"
log() { echo "nsp7-wait-stream: $(date -Is) $*"; }

trap 'log "SIGTERM while waiting; exit 0"; exit 0' TERM INT

if [ ! -x "$BIN" ]; then
  log "FATAL host binary missing/not executable: $BIN"
  exit 1
fi
if [ ! -f "$CFG" ]; then
  log "FATAL host config missing: $CFG"
  exit 1
fi

if ! command -v tcpdump >/dev/null 2>&1; then
  log "WARN tcpdump not found; cannot gate on NSP-first, starting host directly"
  exec "$BIN" run --config "$CFG"
fi

log "waiting for NSP RTP udp/${PORT} on ${IFACE} (NSP-first gate)"
while :; do
  if timeout 2 tcpdump -ni "$IFACE" -q -c 1 "udp dst port ${PORT}" >/dev/null 2>&1; then
    log "NSP stream detected; starting host"
    exec "$BIN" run --config "$CFG"
  fi
  sleep 1
done
