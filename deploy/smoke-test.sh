#!/usr/bin/env bash
# khaen 控制面冒烟测试（设备本机执行，只读）
#
# 用法：
#   ./smoke-test.sh [http://127.0.0.1:8730] [预期协议版本]
#
# 只做 GET：/api/status、/api/device、/api/coeff。
# **不做任何设备写入**（不写 CueMix、不写 ALSA 音量、不 prepare/commit 方案、不出声）。
#
# 退出码：
#   0 = 控制面可达且响应结构符合契约；
#   1 = 请求失败或结构不符；
#   2 = 参数/依赖问题。
#
# 说明：
# - device.wired=false 是**合法状态**（没配置监督器），冒烟测试不把它当失败，只打印原因。
# - 设备真的上报 faulted 时同样只打印；是否算失败由后续会话/人工判断（避免夜间误报）。
set -euo pipefail

BASE="${1:-http://127.0.0.1:8730}"
EXPECT_PROTOCOL="${2:-}"

command -v curl >/dev/null 2>&1 || { echo "missing dependency: curl" >&2; exit 2; }
PYTHON="${PYTHON:-}"
if [[ -z "${PYTHON}" ]]; then
  if command -v python3 >/dev/null 2>&1; then
    PYTHON=python3
  elif command -v python >/dev/null 2>&1; then
    PYTHON=python
  else
    echo "missing dependency: python3 (or PYTHON=...)" >&2
    exit 2
  fi
fi

fail() { echo "FAIL: $*" >&2; exit 1; }

get_json() {
  local path="$1"
  local body
  body="$(curl -fsS --max-time 5 "${BASE}${path}")" || fail "GET ${path} unreachable"
  printf '%s' "${body}"
}

echo "==> GET /api/status"
STATUS="$(get_json /api/status)"
printf '%s' "${STATUS}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
assert doc.get("ok") is True, "ok must be true"
assert isinstance(doc.get("protocol_version"), int), "protocol_version must be an int"
for key in ("runtime_state", "wired", "not_wired", "reasons", "mute", "blocks"):
    assert key in doc, f"missing field: {key}"
print(
    "status: protocol=%s state=%s blocks=%s mute=%s wired=%s"
    % (doc["protocol_version"], doc["runtime_state"], doc["blocks"], doc["mute"], len(doc["wired"]))
)
'
PROTOCOL="$(printf '%s' "${STATUS}" | "${PYTHON}" -c 'import json,sys; print(json.load(sys.stdin)["protocol_version"])')"
if [[ -n "${EXPECT_PROTOCOL}" && "${PROTOCOL}" != "${EXPECT_PROTOCOL}" ]]; then
  fail "protocol_version ${PROTOCOL} != expected ${EXPECT_PROTOCOL}"
fi

echo "==> GET /api/device"
DEVICE="$(get_json /api/device)"
printf '%s' "${DEVICE}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
wired = doc.get("wired")
assert isinstance(wired, bool), "device.wired must be a bool"
if not wired:
    print("device: wired=false reason=%s" % doc.get("reason"))
else:
    assert "state" in doc and "plan_rate" in doc, "wired device must report state and plan_rate"
    rate = doc.get("device_rate")
    safety = doc.get("safety") or {}
    print(
        "device: wired=true state=%s plan_rate=%s device_rate=%s rate_asserted=%s requires_mute=%s"
        % (doc["state"], doc["plan_rate"], rate, doc.get("rate_asserted"), safety.get("requires_mute"))
    )
    if doc.get("last_error"):
        print("device last_error: %s" % json.dumps(doc["last_error"], ensure_ascii=False))
'

echo "==> GET /api/coeff"
COEFF="$(get_json /api/coeff)"
printf '%s' "${COEFF}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
for key in ("version", "last_good_version", "graph_id", "applied_block", "reasons"):
    assert key in doc, f"coeff response missing field: {key}"
print("coeff: version=%s graph_id=%s" % (doc["version"], doc["graph_id"]))
'

echo "OK: control plane reachable and contract-shaped (no device writes performed)"
