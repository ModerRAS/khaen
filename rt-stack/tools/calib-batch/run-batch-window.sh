#!/usr/bin/env bash
# KHAEN 校准批量：板端窗口内一键执行（只跑 plan 冻结命令；不现场生成/不现场分析）
# 用法：run-batch-window.sh <plan.json> <work_dir>
# 退出码：0 成功；2 用法；3 NOT_RUN/阻塞；4 执行失败（已尝试 restore）
set -u
if [ $# -lt 2 ]; then echo "usage: run-batch-window.sh <plan.json> <work_dir>" >&2; exit 2; fi
PLAN="$1"
WORK="$2"
HERE=$(cd "$(dirname "$0")" && pwd)
python3 "$HERE/khaen_cal_batch.py" run --plan "$PLAN" --work "$WORK" --transport cmd
RC=$?
case "$RC" in
  0) echo "BATCH_WINDOW_OK work=$WORK";;
  3) echo "BATCH_WINDOW_NOT_RUN work=$WORK";;
  *) echo "BATCH_WINDOW_FAILED rc=$RC work=$WORK";;
esac
exit "$RC"
