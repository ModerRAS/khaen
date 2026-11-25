#!/usr/bin/env sh
# TSPI RT stack 离线检查（WBS T06）
#
# 用法（在 rt-stack/ 或任意目录）：
#   sh ci/check.sh
#   CARGO_OFFLINE=1 sh ci/check.sh     # 强制 cargo --offline
#
# 检查项：cargo fmt --all --check / cargo test --workspace / rt-check abi-layout /
#         rt-check forbid / rt-check drop-audit；全部离线、无板端动作。
# 退出码：0 = 全部通过；非 0 = 某步失败（set -e 立即停止）。

set -eu
cd "$(dirname "$0")/.."

CARGO_OFFLINE="${CARGO_OFFLINE:-0}"
if [ "$CARGO_OFFLINE" = "1" ]; then
    CARGO_OFFLINE_FLAG="--offline"
else
    CARGO_OFFLINE_FLAG=""
fi

echo "==> cargo fmt --all --check"
cargo fmt --all --check

echo "==> cargo test --workspace"
cargo test $CARGO_OFFLINE_FLAG --workspace

echo "==> rt-check abi-layout"
cargo run $CARGO_OFFLINE_FLAG --quiet -p tspi-rt-check --bin rt-check -- abi-layout

echo "==> rt-check forbid"
cargo run $CARGO_OFFLINE_FLAG --quiet -p tspi-rt-check --bin rt-check -- forbid --root crates

echo "==> rt-check drop-audit"
cargo run $CARGO_OFFLINE_FLAG --quiet -p tspi-rt-check --bin rt-check -- drop-audit --root crates

echo "ALL CHECKS PASS"
