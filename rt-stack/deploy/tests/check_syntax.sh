#!/usr/bin/env bash
# T28 语法检查：bash -n 覆盖 deploy/ 下全部 shell（bin/lib/tests/mocks）；不执行任何部署动作。
# 用法: ./check_syntax.sh [--quiet]；exit 0=全通过，3=有语法错误。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY="$(cd "$HERE/.." && pwd)"
QUIET=0; [ "${1:-}" = "--quiet" ] && QUIET=1
n=0; fail=0
# config/ 下有两个板端 shell（keeper、task-192 irq-affinity-pin）；一并做 bash -n 语法检查。
for f in "$DEPLOY"/bin/*.sh "$DEPLOY"/lib/*.sh "$DEPLOY"/tools/*.sh "$DEPLOY"/tests/*.sh "$DEPLOY"/tests/mocks/* \
         "$DEPLOY"/config/irq-affinity-pin; do
  [ -f "$f" ] || continue
  n=$((n+1))
  if bash -n "$f" 2>/tmp/khaen-t28-bashn.err; then
    [ "$QUIET" = "1" ] || echo "OK   $(basename "$f")"
  else
    echo "FAIL $(basename "$f")"; sed 's/^/     /' /tmp/khaen-t28-bashn.err
    fail=$((fail+1))
  fi
done
rm -f /tmp/khaen-t28-bashn.err
echo "checked=$n failed=$fail"
[ "$fail" -eq 0 ] || exit 3
exit 0
