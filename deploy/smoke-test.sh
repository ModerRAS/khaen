#!/usr/bin/env bash
# khaen 控制面冒烟测试（设备本机执行；只读 + 一次登录）
#
# 用法：
#   KHAEN_SMOKE_PASSWORD='<管理员密码>' ./smoke-test.sh [http://127.0.0.1:8730] [预期 API 版本]
#   KHAEN_SMOKE_TOKEN='<会话 token>'   ./smoke-test.sh ...
#
# 做了什么：
#   - GET /healthz（匿名）；
#   - GET /api/auth/status，未初始化时直接提示去 WebUI setup（不写入任何密码）；
#   - 用 KHAEN_SMOKE_PASSWORD 登录（POST /api/auth/login，只建会话）或用 KHAEN_SMOKE_TOKEN；
#   - 带会话 GET /api/meta、/api/status（khaen-status/v1）、/api/device、/api/coeff。
# 明确不做：不改设备、不写 CueMix/ALSA、不 prepare/commit 方案、不出声。
#
# 退出码：0 = 控制面可达且响应结构符合契约；1 = 请求失败或结构不符；2 = 参数/依赖问题。
# 说明：device.wired=false / rt-host 未连接属合法状态，只打印原因，不判失败。
set -euo pipefail

BASE="${1:-http://127.0.0.1:8730}"
EXPECT_API_VERSION="${2:-}"
PASSWORD="${KHAEN_SMOKE_PASSWORD:-}"
TOKEN="${KHAEN_SMOKE_TOKEN:-}"

command -v curl >/dev/null 2>&1 || { echo "missing dependency: curl" >&2; exit 2; }
PYTHON="${PYTHON:-}"
if [[ -z "${PYTHON}" ]]; then
  if command -v python3 >/dev/null 2>&1; then PYTHON=python3
  elif command -v python >/dev/null 2>&1; then PYTHON=python
  else echo "missing dependency: python3 (or PYTHON=...)" >&2; exit 2; fi
fi

fail() { echo "FAIL: $*" >&2; exit 1; }
JAR="$(mktemp)"
trap 'rm -f "${JAR}"' EXIT

# auth_headers: 无凭据时为空；有 token 时输出 X-KHAEN-Token 头。
auth_headers() { if [[ -n "${TOKEN}" ]]; then printf 'X-KHAEN-Token: %s' "${TOKEN}"; fi; }
get_json() {
  local path="$1"
  local body
  local headers=()
  if [[ -n "${TOKEN}" ]]; then headers=(-H "X-KHAEN-Token: ${TOKEN}"); fi
  body="$(curl -fsS --max-time 5 -b "${JAR}" -c "${JAR}" "${headers[@]}" "${BASE}${path}")" \
    || fail "GET ${path} unreachable"
  printf '%s' "${body}"
}

echo "==> GET /healthz"
HEALTH="$(get_json /healthz)"
printf '%s' "${HEALTH}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
assert set(doc.keys()) == {"ok", "version", "uptime_s"}, f"unexpected healthz keys: {sorted(doc)}"
assert doc["ok"] is True and isinstance(doc["uptime_s"], int)
print("healthz: ok version=%s uptime_s=%s" % (doc["version"], doc["uptime_s"]))
'

echo "==> GET /api/auth/status"
AUTH="$(get_json /api/auth/status)"
printf '%s' "${AUTH}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
assert isinstance(doc.get("configured"), bool), "configured must be bool"
assert isinstance(doc.get("authenticated"), bool), "authenticated must be bool"
print("auth: configured=%s authenticated=%s" % (doc["configured"], doc["authenticated"]))
' || fail "auth/status 响应结构不符"
CONFIGURED="$(printf '%s' "${AUTH}" | "${PYTHON}" -c 'import json,sys; print("1" if json.load(sys.stdin)["configured"] else "0")')"
if [[ "${CONFIGURED}" != "1" ]]; then
  echo "SKIP: 认证尚未初始化（打开 WebUI 走 setup 后再跑本脚本）"
  echo "OK: control plane reachable (healthz/auth-status only; run setup first)"
  exit 0
fi

if [[ -z "${TOKEN}" ]]; then
  [[ -n "${PASSWORD}" ]] || fail "需要 KHAEN_SMOKE_PASSWORD（或 KHAEN_SMOKE_TOKEN）才能检查受保护端点"
  echo "==> POST /api/auth/login"
  LOGIN_BODY="$(printf '{"password":"%s"}' "${PASSWORD}")"
  LOGIN_CODE="$(curl -sS --max-time 5 -o /dev/null -w '%{http_code}' -b "${JAR}" -c "${JAR}" \
    -H 'Content-Type: application/json' -d "${LOGIN_BODY}" "${BASE}/api/auth/login")" \
    || fail "POST /api/auth/login unreachable"
  [[ "${LOGIN_CODE}" == "200" ]] || fail "login failed: HTTP ${LOGIN_CODE}（密码错误=401，限速=429）"
  echo "login: ok"
fi

echo "==> GET /api/meta"
META="$(get_json /api/meta)"
printf '%s' "${META}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
for key in ("api_version", "web_version", "ops", "surfaces", "not_wired"):
    assert key in doc, f"meta missing field: {key}"
assert isinstance(doc["api_version"], int) and isinstance(doc["ops"], list)
print("meta: api_version=%s web_version=%s ops=%s surfaces=%s not_wired=%s"
      % (doc["api_version"], doc["web_version"], len(doc["ops"]), len(doc["surfaces"]), len(doc["not_wired"])))
'
if [[ -n "${EXPECT_API_VERSION}" ]]; then
  API_VERSION="$(printf '%s' "${META}" | "${PYTHON}" -c 'import json,sys; print(json.load(sys.stdin)["api_version"])')"
  [[ "${API_VERSION}" == "${EXPECT_API_VERSION}" ]] || fail "api_version ${API_VERSION} != expected ${EXPECT_API_VERSION}"
fi

echo "==> GET /api/status"
STATUS="$(get_json /api/status)"
printf '%s' "${STATUS}" | "${PYTHON}" -c '
import json, sys
doc = json.load(sys.stdin)
assert doc.get("schema") == "khaen-status/v1", "schema must be khaen-status/v1"
top = {"schema", "daemon", "host_link", "state", "plan", "format", "input", "output",
       "device", "calibration", "not_wired", "reasons"}
assert set(doc.keys()) == top, "unexpected top-level keys: %s" % sorted(set(doc) ^ top)
daemon = doc["daemon"]
for key in ("version", "uptime_s", "bind"):
    assert key in daemon, f"status.daemon missing: {key}"
for key in ("state", "plan", "format", "input", "output", "device", "calibration"):
    assert isinstance(doc[key], dict), f"status.{key} must be an object"
assert isinstance(doc["host_link"], dict) and isinstance(doc["host_link"].get("connected"), bool)
assert isinstance(doc["reasons"], dict) and isinstance(doc["not_wired"], list)
print("status: state=%s muted=%s safe=%s plan_rate=%s graph_id=%s host_link.connected=%s age_ms=%s"
      % (doc["state"].get("value"), doc["state"].get("muted"), doc["state"].get("safe"),
         doc["format"].get("plan_rate"), doc["plan"].get("graph_id"),
         doc["host_link"]["connected"], doc["host_link"].get("status_age_ms")))
'

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
    safety = doc.get("safety") or {}
    print("device: wired=true state=%s plan_rate=%s device_rate=%s rate_asserted=%s requires_mute=%s"
          % (doc["state"], doc["plan_rate"], doc.get("device_rate"), doc.get("rate_asserted"),
             safety.get("requires_mute")))
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
