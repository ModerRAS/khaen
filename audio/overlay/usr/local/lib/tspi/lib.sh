#!/usr/bin/env bash
# lib.sh — tspi-audio 脚本公共函数（tspi-mode / tspi-ptp-status / hooks 共享）
# 约定：不要 set -e（由调用方决定错误处理）；函数失败用 return 1。

TSPI_RUN_DIR="${TSPI_RUN_DIR:-/run/tspi}"
TSPI_STATE_FILE="$TSPI_RUN_DIR/state.json"
TSPI_MODE_FILE="$TSPI_RUN_DIR/mode"
TSPI_LOCK_FILE="$TSPI_RUN_DIR/ptp-lock"

tspi_log() { printf "[tspi] %s\n" "$*" >&2; logger -t tspi-audio -- "$*" 2>/dev/null || true; }
tspi_warn() { printf "[tspi][warn] %s\n" "$*" >&2; logger -t tspi-audio -p warning -- "$*" 2>/dev/null || true; }
tspi_die() { printf "[tspi][error] %s\n" "$*" >&2; logger -t tspi-audio -p err -- "$*" 2>/dev/null || true; exit "${2:-1}"; }

# 读默认值（/etc/default/tspi-audio 可覆盖）
tspi_load_defaults() {
  # shellcheck disable=SC1091
  [[ -f /etc/default/tspi-audio ]] && . /etc/default/tspi-audio
  : "${TSPI_IFACE:=eth0}"
  : "${TSPI_MOTU_CARD_ID:=motu}"
  : "${TSPI_RAVENNA_CARD_ID:=ravenna}"
  : "${TSPI_CHANNELS:=16}"
  : "${TSPI_MAX_PLAYBACK_CHANNELS:=22}"
  : "${TSPI_LAYOUT:=5.3.2}"
  : "${TSPI_OUTPUTS:=10}"
  : "${TSPI_DANTE_SAMPLERATE:=96000}"
  : "${TSPI_AES67_SAMPLERATE:=48000}"
  : "${TSPI_DANTE_CHUNKSIZE:=64}"
  : "${TSPI_AES67_CHUNKSIZE:=64}"
  : "${TSPI_DANTE_TARGET_LEVEL:=256}"
  : "${TSPI_AES67_TARGET_LEVEL:=256}"
  : "${TSPI_ADJUST_PERIOD:=10}"
  : "${TSPI_AES67_PTP_ROLE:=slave}"
  : "${TSPI_WAIT_PTP_SECS:=20}"
  : "${TSPI_WAIT_DEVICE_SECS:=10}"
  : "${TSPI_VERIFY_SECS:=3}"
  : "${TSPI_LOCK_THRESHOLD_NS:=1000000}"
  : "${TSPI_ROLLBACK:=1}"
  : "${TSPI_RX_LATENCY_NS:=4000000}"
  : "${TSPI_TX_LATENCY_NS:=1000000}"
  : "${TSPI_INFERNO_NAME:=tspi-dsp}"
  : "${TSPI_INFERNO_DEVICE_ID:=auto}"
  : "${TSPI_CAMILLADSP_PORT:=1234}"
  : "${TSPI_CAMILLADSP_BIND:=127.0.0.1}"
  : "${TSPI_ALLOW_UNLOCKED_PTP:=0}"
  export TSPI_IFACE TSPI_MOTU_CARD_ID TSPI_RAVENNA_CARD_ID TSPI_CHANNELS TSPI_MAX_PLAYBACK_CHANNELS TSPI_LAYOUT TSPI_OUTPUTS \
         TSPI_DANTE_SAMPLERATE TSPI_AES67_SAMPLERATE TSPI_DANTE_CHUNKSIZE TSPI_AES67_CHUNKSIZE \
         TSPI_DANTE_TARGET_LEVEL TSPI_AES67_TARGET_LEVEL TSPI_ADJUST_PERIOD TSPI_AES67_PTP_ROLE \
         TSPI_WAIT_PTP_SECS TSPI_WAIT_DEVICE_SECS TSPI_VERIFY_SECS TSPI_LOCK_THRESHOLD_NS TSPI_ROLLBACK \
         TSPI_RX_LATENCY_NS TSPI_TX_LATENCY_NS TSPI_INFERNO_NAME TSPI_INFERNO_DEVICE_ID \
         TSPI_CAMILLADSP_PORT TSPI_CAMILLADSP_BIND TSPI_ALLOW_UNLOCKED_PTP
}

# /proc/asound/cards 里按 card id（方括号内）或名称匹配，返回 card 序号；找不到返回空
tspi_card_index() {
  local want_id="${1:-}" want_name="${2:-}"
  [[ -r /proc/asound/cards ]] || { echo ""; return 0; }
  awk -v id="$want_id" -v name="$want_name" '
    /^[ \t]*[0-9]+[ \t]+\[/ {
      idx=$1;
      line=$0;
      sub(/^[^[]*\[/, "", line);
      sub(/\].*$/, "", line);
      gsub(/[ \t]+$/, "", line);
      gsub(/^[ \t]+/, "", line);
      if (tolower(line) == tolower(id)) { print idx; found=1; exit }
      if (name != "" && index($0, name) > 0) { print idx; found=1; exit }
    }
    END { if (!found) print "" }' /proc/asound/cards
}

# 用 aplay/arecord --dump-hw-params 探测设备的通道数范围与格式列表
# 输出：CHANNELS_MIN CHANNELS_MAX FORMATS（FORMATS 为空格分隔 ALSA 格式名）
tspi_probe_pcm() {
  local dir="$1" card="$2" dev="${3:-0}" tool tmp err
  if [[ "$dir" == "playback" ]]; then tool="aplay"; else tool="arecord"; fi
  command -v "$tool" >/dev/null 2>&1 || return 1
  tmp="$(mktemp)"; err="$(mktemp)"
  "$tool" -D "hw:$card,$dev" --dump-hw-params /dev/zero >"$tmp" 2>"$err" || true
  local ch fmts dump
  dump="$(cat "$tmp" "$err" 2>/dev/null)"
  ch="$(printf "%s" "$dump" | awk -F: '/^CHANNELS:/ {sub(/^[ \t]+/, "", $2); print $2}' | head -1)"
  fmts="$(printf "%s" "$dump" | awk -F: '/^FORMAT:/ {sub(/^[ \t]+/, "", $2); print $2}' | head -1)"
  rm -f "$tmp" "$err"
  [[ -n "$ch" ]] || return 1
  # ALSA dump 可能是 "2"、"[2 22]"、"2 22"、"2-22"、"[2-22]"
  ch="${ch//[/}"; ch="${ch//]/}"
  local chmin chmax
  if [[ "$ch" == *-* ]]; then
    chmin="${ch%%-*}"; chmax="${ch##*-}"
  elif [[ "$ch" == *" "* ]]; then
    read -r chmin chmax <<<"$ch"
  else
    chmin="$ch"; chmax="$ch"
  fi
  [[ "$chmin" =~ ^[0-9]+$ && "$chmax" =~ ^[0-9]+$ ]] || return 1
  echo "$chmin $chmax $fmts"
}

# 把 ALSA 格式名映射为 CamillaDSP format 名；不存在返回空
tspi_camilla_format() {
  case "$1" in
    S24_3LE) echo "S24_3_LE" ;;
    S24_LE)  echo "S24_4_LE" ;;
    S32_LE)  echo "S32_LE" ;;
    S16_LE)  echo "S16_LE" ;;
    FLOAT_LE) echo "F32_LE" ;;
    *) echo "" ;;
  esac
}

# 服务是否 active
tspi_svc_active() { systemctl is-active --quiet "$1" 2>/dev/null; }

# 等待服务 active，成功 0 失败 1
tspi_wait_svc() {
  local svc="$1" timeout="${2:-10}" i=0
  while (( i < timeout * 2 )); do
    if tspi_svc_active "$svc"; then return 0; fi
    sleep 0.5; i=$((i+1))
  done
  return 1
}

# 等待 ALSA 卡出现，成功 0 失败 1
tspi_wait_card() {
  local id="$1" name="$2" timeout="${3:-10}" i=0
  while (( i < timeout * 2 )); do
    if [[ -n "$(tspi_card_index "$id" "$name")" ]]; then return 0; fi
    sleep 0.5; i=$((i+1))
  done
  return 1
}

# AES67 daemon PTP 状态：优先 /run/tspi/ptp-lock，其次 REST
tspi_aes67_ptp_status() {
  if [[ -r "$TSPI_LOCK_FILE" ]]; then
    local s; s="$(head -1 "$TSPI_LOCK_FILE" 2>/dev/null | tr -d "[:space:]")"
    [[ -n "$s" ]] && { echo "$s"; return 0; }
  fi
  if command -v curl >/dev/null 2>&1; then
    local out; out="$(curl -fsS --max-time 2 http://127.0.0.1:8080/api/ptp/status 2>/dev/null || true)"
    if [[ -n "$out" ]]; then
      printf "%s" "$out" | jq -r '.ptp_status // .status // empty' 2>/dev/null && return 0
    fi
  fi
  echo "unknown"
  return 1
}

# statime 观测 socket -> 一行 JSON（失败返回空）
tspi_statime_observe() {
  local sock="/run/tspi/statime-observe.sock"
  [[ -S "$sock" ]] || return 1
  if command -v nc >/dev/null 2>&1; then
    timeout 3 nc -U -w 2 "$sock" 2>/dev/null
  elif command -v socat >/dev/null 2>&1; then
    timeout 3 socat - UNIX-CONNECT:"$sock" 2>/dev/null
  else
    return 1
  fi
}

# 读 state.json 字段（jq），不存在返回空
tspi_state_get() {
  [[ -r "$TSPI_STATE_FILE" ]] || { echo ""; return 0; }
  jq -r --arg k "$1" '.[$k] // empty' "$TSPI_STATE_FILE" 2>/dev/null || echo ""
}

# bogus 保护：在本文件被重复 source 时避免函数重定义告警（无操作）
true
