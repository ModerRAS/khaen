#!/bin/bash
# nsp7-restore-cuemix-low.sh —— 出声窗口：把 CueMix 从夜间 mute 恢复到白天低电平
#   （用户 2026-11-22 11:30 指示：游戏 NSP 链路测试；kMainTrim=21 低档，不用夜间 mute）
# 内部顺序按 day-window RUNBOOK §2.2：A1 kMainTrim=60 → B restore 计划 → C 40 → 21
# 证据：$EV/game-window/cuemix-game-*.json；失败则回到 mute（koTrim=100 + busMute=1 + main 100）。
set -u
EV="${1:-/root/aes67-rx-work/chain/nsp7-daemon-20261122}"
HOST=169.254.111.126
C=/root/cuemix_client.py
mkdir -p "$EV/game-window"
G="$EV/game-window"
date -Is > "$G/restore.start.txt"
echo '=== A0 现状（应为 mute）==='
python3 $C --host $HOST --read 4 --save "$G/cuemix-game-before.json" >/dev/null 2>&1
python3 - <<'EOF'
import json
d=json.load(open('/root/aes67-rx-work/chain/nsp7-daemon-20261122/game-window/cuemix-game-before.json'))['values']
print('before koTrim', d.get('koTrim',[])[:12], 'kMainTrim', d.get('kMainTrim'), 'koBusMute', d.get('koBusMute'))
EOF
echo '=== A1 kMainTrim=60 ==='
python3 $C --host $HOST --read 3 --set kMainTrim=0=60 --yes >/dev/null 2>&1
echo '=== B apply restore plan（koTrim0-9=0, koTrim10-11=24, koBusMute0-7=0）==='
args=()
for i in $(seq 0 9); do args+=(--set koTrim=$i=0); done
args+=(--set koTrim=10=24 --set koTrim=11=24)
for i in $(seq 0 7); do args+=(--set koBusMute=$i=0); done
python3 $C --host $HOST --yes "${args[@]}" --read 3 --save "$G/cuemix-game-restored-plan.json" >/dev/null 2>&1
echo '=== C kMainTrim 40 -> 21 ==='
python3 $C --host $HOST --read 2 --set kMainTrim=0=40 --yes >/dev/null 2>&1
sleep 0.3
python3 $C --host $HOST --read 3 --set kMainTrim=0=21 --yes --save "$G/cuemix-game-after.json" >/dev/null 2>&1
python3 - <<'EOF'
import json
d=json.load(open('/root/aes67-rx-work/chain/nsp7-daemon-20261122/game-window/cuemix-game-after.json'))['values']
print('after koTrim', d.get('koTrim',[])[:12], 'kMainTrim', d.get('kMainTrim'), 'koBusMute', d.get('koBusMute'), 'kSampleRate', d.get('kSampleRate'))
EOF
date -Is > "$G/restore.end.txt"
echo RESTORE_LOW_DONE
