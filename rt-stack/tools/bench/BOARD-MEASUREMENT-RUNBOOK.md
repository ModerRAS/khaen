# 板端性能测量 runbook —— T29 / T46 / T30（板窗专用，当前 NOT_RUN）

> 任务：team task-156（据此准备板端测量方案，不占板、不执行）。执行者：**唯一 e2e-runner**
> 在板 owner 授权的独占窗口按本文执行。写域：本文件与 evidence/perf-harness/；板端证据由
> e2e-runner 落到 evidence/rt-probes/（T29）、evidence/multicore/（T46）、evidence/soak/（T30）。
>
> 当前事实（2026-11-17）：开发期 S1/CamillaDSP 仍在板上运行，**本任务不打断、不切换、不退役**；
> 以下所有命令均为**未来窗口计划**。WBS 状态：T29/T46/T30 全部 NOT_RUN。
>
> 前置真实现状（必须写进窗口前检查，缺一即阻塞相应测量项）：
> - ALSA 真实 backend 仍 NOT_WIRED（T155 host 只有 fake backend）；T30 的“已校准生产图 +
>   真实播放”依赖 T26/T17 follow-up，未接前只能测 kernel/DAG 或 fake 链。
> - 四 worker FFT 计划（输入 FFT/频点/输出 IFFT 分派）尚未实现；T46 若需要 FFT profile，
>   依赖 T08/T26/T56 follow-up。
> - T20 crossfade 只到 swap-ack 层；T21 事件未接 UI；T23/T25/T56 整链入口未接（见 README §10）。
> - 板端 bench 二进制：当前 rt-stack 无 board-side bench CLI；T47（crates/bench）仍是 scaffold。
>   需要先把 tools/bench/src/perf_harness.rs 的 board 变体或 T47 版本交叉编译进 aarch64
>   release（T149 的 multiarch sysroot 构建链可用），否则 T46 只能按 host metrics 的次优口径
>   记录并标 NOT_VERIFIED。
> - 板端测量必须独占：任何 cyclictest/osnoise/满载/allocation 压测都会干扰正在播放的 S1，
>   **不叫只读快照**。排期前先与板 owner 确认空窗与回滚点。

## 0. 窗口前检查（一次性，全部落 evidence）

~~~bash
# 0.1 环境与 RT 内核
uname -a; cat /proc/cmdline; cat /sys/kernel/realtime 2>/dev/null || echo NO_REALTIME_NODE
lscpu -e=CPU,CORE,SOCKET,ONLINE,MAXMHZ,MINMHZ
lscpu | sed -n '1,40p'
cat /sys/devices/system/cpu/isolated 2>/dev/null; cat /sys/devices/system/cpu/nohz_full 2>/dev/null
cat /proc/sys/kernel/sched_rt_runtime_us; cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | sort -u
cat /proc/interrupts > before-interrupts.txt
systemctl list-units --type=service --no-pager | grep -Ei 'camilla|khaen|aes67|ptp|alsa' || true
pgrep -a camilladsp || echo NO_CAMILLA_PROCESS
ls /sys/class/thermal/thermal_zone*/temp 2>/dev/null; uptime

# 0.2 板 owner 授权与回滚点（记录在窗口 MANIFEST）
#   - 授权人/时间窗/可打断的播放/恢复命令/当前 S1 service 与 last-good 版本
#   - 记录当前播放状态（若在播，按 owner 指示暂停或选空窗）
~~~

判据：若 /sys/kernel/realtime 缺失、内核非 PREEMPT_RT、或无人授权，则 T29 直接 NOT_RUN（不是
FAIL）；不得在非 RT 内核上宣称 RT 门。

## 1. T29：RT / IRQ / cyclictest / osnoise 专窗（100us 工程门）

目标：正式 RT-first 基线，不与 P1 混记；**100us 工程门**（cyclictest/osnoise max），
同时记录 0-load 与本机满载两种状态；不回溯冒充 P1 实测。

~~~bash
# 1.1 idle 基线（3 次重复，每次 5 min；先记录温度/频率）
for i in 1 2 3; do
  taskset -c 6 chrt -f 95 cyclictest -S -m -p 95 -i 200 -d 0 -D 5m -h 400 -q > cyclictest-idle-$i.txt 2>&1
  cat /sys/class/thermal/thermal_zone*/temp >> thermal-idle-$i.txt
done

# 1.2 满载（hackbench 或其他已批准负载；进程/CPU 必须可复述）
hackbench -l 100000 -g 8 -P &
LOAD_PID=$!
taskset -c 6 chrt -f 95 cyclictest -S -m -p 95 -i 200 -d 0 -D 5m -h 400 -q > cyclictest-load-1.txt 2>&1
wait $LOAD_PID

# 1.3 osnoise（若内核提供；tracefs 写入属主动操作，需在授权窗口）
ls /sys/kernel/tracing/osnoise 2>/dev/null || echo NO_OSNOISE
# 例：osnoise 采样 1m，记录 max/avg；具体参数按内核版本与 T29 owner 计划冻结后写入本 runbook

# 1.4 IRQ 亲和与抖动来源（只读采集）
cat /proc/interrupts > after-interrupts.txt
ps -eLo pid,tid,psr,prio,rtprio,comm | grep -E 'irq/|ktimers|rcu' | head -50
~~~

判定：cyclictest max < **100 us**（工程门）且 osnoise 满载 max 同门；报告 3 次重复的 max 分布，
不得只报最好一次。任一超门 → 记录 FAIL 与当时频率/温度/IRQ/负载，不现场调参掩盖。
失败处理：任一步骤导致播放异常/音频中断 → 立即停止、恢复 owner 指定状态、保存现场；
本项记 NOT_RUN 或 FAIL，不重试直到定位。

## 2. T46：同 kernel 1/2/4 worker wall speedup ≥3.2（正式多核验收）

口径（DECISION §4 / WBS T46）：
- 同 kernel/算法/precision/coeff/input hash/fs/B/compiler/ISA/热身/驻留/频率/亲和；
  **只改 1/2/4 worker**；禁止 1 核 direct 对 4 核 FFT、f64 对 f32。
- speedup4 = T1_wall / T4_wall（同一统计量，中位数与尾值都报）；要求 **≥3.2×**。
- 每 worker 报 busy_compute、wait_barrier/spin、max_kernel、queue、迁移/cache 统计；
  load_k=有用计算/窗口，CV=std/mean，range=(max−min)/mean；长滤波 steady 默认
  **CV≤0.15、range≤0.30**；空/小图 N/A 要写理由，不能 spin 凑 100%。
- kernel 3.2× 与整链 Amdahl 分开报告（S_total=1/(s+(1−s)/4)）；不拿 kernel 加速当总链。
- 128 满图 worst<1.3333ms、p99.9 同门、0 miss；64 帧独立窗<0.6667ms。

执行框架（需先有 board bench 二进制，见前置）：
~~~bash
# 2.1 记录并冻结环境（同 T29 0.x）+ 确认 dsp_cpus/topology（T45 scheduler 参数化计划）
#     默认 deploy 模板：dsp_cpus="4-7"（A72 全核；2-5 混 A53/A72 实测慢 3.1×）、
#     dsp_priority=70、io=80、irq=90、apply_affinity=false；
#     board 窗口由 host 启动期按 T45 计划实际写入（不是本 runbook 自动写内核）。

# 2.2 1/2/4 worker，每档 3 重复；同一输入/系数/图（hash 落盘）
for W in 1 2 4; do
  for R in 1 2 3; do
    taskset -c 4-7 khaen-bench-dag --workers $W --block 128 --blocks 300 --warmup 30 \
      --graph matrix_prod_g4096_p1 --out multicore-W$W-R$R.csv 2>&1 | tee multicore-W$W-R$R.log
  done
done

# 2.3 64 帧独立窗：同命令 --block 64（单独窗口，不复用 128 数据）
# 2.4 记录：cat /proc/interrupts、频率、温度、per-worker busy/wait/max_kernel、queue、
#           xrun/miss 计数；宿主 CPU 占用（pidstat -t -p $(pgrep khaen-rt-host) 1 60）
~~~

判定：speedup4≥3.2 且 128/64 门通过 → 该项 PASS；任一不满足 → FAIL/NOT_VERIFIED，
附静态负载分析（本工装已发现当前 workers.rs lane 分配 W4=2/2/2/1、纯算术上限 3.5×，
板端若仍 2:1 不均，3.2× 在数学上不可达 —— 先修 kernel 再约窗）。
失败处理：<3.2 时先核对是否同 kernel/系数/亲和/温控；确认不是测量错误后记录 FAIL，
不得换更短 profile 重跑当 PASS；不得用平均 CPU 或 FLOP 模型替代实测。

## 3. T30：已校准生产图 4 worker 128/48k 1h 0 xrun + 64 独立窗 + 冷启/切换恢复

前置：真实 ALSA/MOTU 打开、真实校准生产图（5×5 coeff + 对角 P）、唯一 I/O owner、
T20 swap + T21 fault + T45 亲和已接线（当前均 NOT_WIRED/NOT_RUN）。

~~~bash
# 3.1 soak 启动（示例；真实参数以 deploy/config/rt-host.toml 冻结值为准）
khaen-rt-host run --config /etc/khaen-rt-stack/rt-host.toml --duration-ms 3600000 \
  > soak-1h-run1.log 2>&1 &
HOST_PID=$!
# 同时每 10 s 采样：温度/频率、/proc/interrupts 差值、进程 RSS、per-worker 统计、
# 队列水位、xrun/deadline-miss 计数（由 T21 事件/控制面导出，未接前用 journald + proc 快照）
sleep 3600; wait $HOST_PID; echo exit=$?

# 3.2 64 帧独立窗（另一次 1h 或计划要求的窗口；独立记录，不复用 128 数据）
# 3.3 冷启：stop -> 冷启动 -> 首块延迟/首帧 timestamp/无半图/无未 ack
# 3.4 热切/切换恢复：prepare->apply->all-ack->quarantine->retire 全链 + crossfade 预算
#     （双图 CPU/RAM 峰值；D 对齐；超预算必须 ERR_CROSSFADE_BUDGET/RT_E_CROSSFADE_ALIGN）
# 3.5 故障/回退：人为失核/超时 -> 整块 invalid -> fallback（G=I 保 P_post）-> 安全
#     mute 路径；确认不输出半图、不继续旧图、不丢 quarantine。
~~~

判定：1h 内 **0 xrun、0 deadline miss**、worst<1.3333ms（p99.9 同门）、64 独立窗<0.6667ms、
冷启/切换/回退全部有记录且无半图/无未 ack → PASS；任一不满足 → FAIL/NOT_VERIFIED 并附现场。
失败处理：首次 xrun/deadline miss 即停止计时、保存证据、按 owner 指示安全静音/回滚；
不得删日志/重跑覆盖；不得用“平均 CPU 很低”反驳 xrun。

## 4. 证据与判定规范

- 每个窗口一个目录：evidence/rt-probes/<ts>/、evidence/multicore/<ts>/、evidence/soak/<ts>/；
  含命令原文、exit code、原始 CSV/JSON、环境快照、频率/温度/IRQ、图/系数 hash、MANIFEST.md、
  SHA256SUMS、NOT_RUN 清单与失败分析。
- 只有 e2e-runner 可写板端证据；leaf/评审只读。
- 未测项一律 NOT_RUN / NOT_VERIFIED，不得在 P1/正式报告里写成 PASS（DECISION §7：
  G-performance 单独判定，不绑 P1 签收）。
- 任何门失败保留 FAIL + 现场，修复后另开窗口，不覆盖历史证据。

## 5. 本任务已准备好/未准备的部分

已准备（本仓库）：
- 离线工装 tools/bench（工装链路、统计口径、bit-exact 自检、CSV/JSON schema、CPU 预算核算）；
- 本 runbook 的命令/判据/失败处理框架；
- 本机 x86 参考数据（evidence/perf-harness），用于先期暴露 kernel/负载问题（见 CPU-BUDGET-ACCOUNTING.md §3/§4）。

未准备（NOT_RUN，窗口前必须补齐）：
- board-side bench 二进制与 aarch64 发布（依赖 T47/T26 follow-up 或 tools 工装交叉编译）；
- 真实 ALSA/MOTU backend、真实校准图加载（T17/T26 flank）；
- 四 worker FFT 计划实现（T08/T26/T56）；
- T20/T21/T45 在真实板端的热切/故障/亲和接线验证。
