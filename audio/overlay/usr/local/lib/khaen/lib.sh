#!/usr/bin/env bash
# lib.sh — khaen-audio 脚本公共函数（khaen-mode / khaen-ptp-status / hooks 共享）
# 约定：不要 set -e（由调用方决定错误处理）；函数失败用 return 1。

KHAEN_RUN_DIR="${KHAEN_RUN_DIR:-/run/khaen}"
KHAEN_STATE_FILE="$KHAEN_RUN_DIR/state.json"
KHAEN_MODE_FILE="$KHAEN_RUN_DIR/mode"
KHAEN_LOCK_FILE="$KHAEN_RUN_DIR/ptp-lock"

khaen_log() { printf "[khaen] %s\n" "$*" >&2; logger -t khaen-audio -- "$*" 2>/dev/null || true; }
khaen_warn() { printf "[khaen][warn] %s\n" "$*" >&2; logger -t khaen-audio -p warning -- "$*" 2>/dev/null || true; }
khaen_die() { printf "[khaen][error] %s\n" "$*" >&2; logger -t khaen-audio -p err -- "$*" 2>/dev/null || true; exit "${2:-1}"; }

# 读默认值（/etc/default/khaen-audio 可覆盖）
khaen_load_defaults() {
  # shellcheck disable=SC1091
  [[ -f /etc/default/khaen-audio ]] && . /etc/default/khaen-audio
  : "${KHAEN_IFACE:=eth0}"
  : "${KHAEN_MOTU_CARD_ID:=motu}"
  : "${KHAEN_RAVENNA_CARD_ID:=ravenna}"
  : "${KHAEN_CHANNELS:=16}"
  : "${KHAEN_MAX_PLAYBACK_CHANNELS:=22}"
  : "${KHAEN_LAYOUT:=5.3.2}"
  : "${KHAEN_OUTPUTS:=10}"
  : "${KHAEN_DANTE_SAMPLERATE:=96000}"
  : "${KHAEN_AES67_SAMPLERATE:=48000}"
  : "${KHAEN_DANTE_CHUNKSIZE:=64}"
  : "${KHAEN_AES67_CHUNKSIZE:=64}"
  : "${KHAEN_DANTE_TARGET_LEVEL:=256}"
  : "${KHAEN_AES67_TARGET_LEVEL:=256}"
  : "${KHAEN_ADJUST_PERIOD:=10}"
  : "${KHAEN_AES67_PTP_ROLE:=slave}"
  : "${KHAEN_WAIT_PTP_SECS:=20}"
  : "${KHAEN_WAIT_DEVICE_SECS:=10}"
  : "${KHAEN_VERIFY_SECS:=3}"
  : "${KHAEN_LOCK_THRESHOLD_NS:=1000000}"
  : "${KHAEN_ROLLBACK:=1}"
  : "${KHAEN_RX_LATENCY_NS:=4000000}"
  : "${KHAEN_TX_LATENCY_NS:=1000000}"
  : "${KHAEN_INFERNO_NAME:=khaen-dsp}"
  : "${KHAEN_INFERNO_DEVICE_ID:=auto}"
  : "${KHAEN_CAMILLADSP_PORT:=1234}"
  : "${KHAEN_CAMILLADSP_BIND:=127.0.0.1}"
  : "${KHAEN_ALLOW_UNLOCKED_PTP:=0}"
  export KHAEN_IFACE KHAEN_MOTU_CARD_ID KHAEN_RAVENNA_CARD_ID KHAEN_CHANNELS KHAEN_MAX_PLAYBACK_CHANNELS KHAEN_LAYOUT KHAEN_OUTPUTS \
         KHAEN_DANTE_SAMPLERATE KHAEN_AES67_SAMPLERATE KHAEN_DANTE_CHUNKSIZE KHAEN_AES67_CHUNKSIZE \
         KHAEN_DANTE_TARGET_LEVEL KHAEN_AES67_TARGET_LEVEL KHAEN_ADJUST_PERIOD KHAEN_AES67_PTP_ROLE \
         KHAEN_WAIT_PTP_SECS KHAEN_WAIT_DEVICE_SECS KHAEN_VERIFY_SECS KHAEN_LOCK_THRESHOLD_NS KHAEN_ROLLBACK \
         KHAEN_RX_LATENCY_NS KHAEN_TX_LATENCY_NS KHAEN_INFERNO_NAME KHAEN_INFERNO_DEVICE_ID \
         KHAEN_CAMILLADSP_PORT KHAEN_CAMILLADSP_BIND KHAEN_ALLOW_UNLOCKED_PTP
}

# /proc/asound/cards 里按 card id（方括号内）或名称匹配，返回 card 序号；找不到返回空
khaen_card_index() {
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
khaen_probe_pcm() {
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
khaen_camilla_format() {
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
khaen_svc_active() { systemctl is-active --quiet "$1" 2>/dev/null; }

# 等待服务 active，成功 0 失败 1
khaen_wait_svc() {
  local svc="$1" timeout="${2:-10}" i=0
  while (( i < timeout * 2 )); do
    if khaen_svc_active "$svc"; then return 0; fi
    sleep 0.5; i=$((i+1))
  done
  return 1
}

# 等待 ALSA 卡出现，成功 0 失败 1
khaen_wait_card() {
  local id="$1" name="$2" timeout="${3:-10}" i=0
  while (( i < timeout * 2 )); do
    if [[ -n "$(khaen_card_index "$id" "$name")" ]]; then return 0; fi
    sleep 0.5; i=$((i+1))
  done
  return 1
}

# AES67 daemon PTP 状态：优先 /run/khaen/ptp-lock，其次 REST
khaen_aes67_ptp_status() {
  if [[ -r "$KHAEN_LOCK_FILE" ]]; then
    local s; s="$(head -1 "$KHAEN_LOCK_FILE" 2>/dev/null | tr -d "[:space:]")"
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
khaen_statime_observe() {
  local sock="/run/khaen/statime-observe.sock"
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
khaen_state_get() {
  [[ -r "$KHAEN_STATE_FILE" ]] || { echo ""; return 0; }
  jq -r --arg k "$1" '.[$k] // empty' "$KHAEN_STATE_FILE" 2>/dev/null || echo ""
}

# bogus 保护：在本文件被重复 source 时避免函数重定义告警（无操作）
true
