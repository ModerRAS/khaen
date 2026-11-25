# T19 clock 私有测试（crates/rt-host/tests/T19）

WBS T19（clock：MOTU frame/PTP/mono 映射 + ppm/uncertainty + 唯一 servo + 失锁新 epoch）
的离线模型测试，经 src/clock.rs 末尾

    #[cfg(test)]
    mod clock_tests { include!("../tests/T19/clock_models.rs"); }

进入 lib 单测构建（cargo 不自动发现 tests/<子目录>；公共注册属 integrator，不要写入
tests/public_registration.rs）。

## 运行

~~~powershell
cd C:\WorkSpace\Claw\projects\khaen-audio-dsp\rt-stack
cargo test -p khaen-rt-host clock --offline
# 期望：60 passed; 0 failed（DoD executed test_count > 0）
~~~

全部测试名 t19_ 前缀，模块路径 clock::clock_tests 含 clock，可用 clock 过滤。
只读取当前离线数据/合成时间戳：不打开声卡、不碰 MOTU/PTP/AoIP 设备、不访问板端。

## 测试分组（60 个）

| 组 | 覆盖 |
|---|---|
| 常量/错误码 | 冻结码映射（104/103/120/204/403/116/407/405）、时域/owner 名、预算常量 |
| mono/tick | CLOCK_MONOTONIC 单调+推进、raw tick 单调、TickBudget<=3、ctl 预算、ticks_to_ns 向量、admission 标定+全局安装、probe_clock_support |
| 三域映射 | frame/mono 已知向量与正负 ppm 方向、PTP 已知向量与往返、三域组合、未锁 PTP 拒绝、采样率/ppm/uncertainty 合理性门、时间戳越界/溢出、1e9 帧无累积舍入漂移、KhaenEpochInfoV1 flags |
| ppm 估计 | 合成 +12.5 ppm/-7.5 ppm/0 ppm、PTP 独立 25 ppm、窗口容量/dropped、点数不足、超 1000 ppm 拒绝、噪声增大 sigma、PTP 无效点 |
| epoch | 起始/单调 id/永不复用/重复 begin/先 discontinuity 拒绝/flags |
| 唯一 servo | 第二 owner 拒绝、无 grant/旧 generation、锁定、holdover+超时失锁、holdover 恢复、质量连击失锁、5 ms step 失锁、uncertainty 门、owner 域隔离、确定性重放 |
| runtime | start/reanchor/epoch_info、唯一 servo、owner 域只写回一个域、超时/step 新 epoch、显式 DeviceReopen、reanchor 前不写 mapping、epoch mismatch 闸门、计数、锁定后往返 |

## 合成漂移/失锁向量

- 时钟模型：mono = start + floor(frame * (1e9 - frame_ppm_ppb) / fs)（有理数一次乘除）；
  PTP = ptp_start + floor((mono-start) * (1e9 + ptp_ppm_ppb) / 1e9)。
- 漂移：+12.5 ppm、-7.5 ppm、0 ppm、PTP +25 ppm；估计容差 100/200 ppb。
- step：锁定后注入 +5 ms PTP 偏移（> step_limit 1 ms）=> PtpStep 失锁 => 新 epoch。
- 超时：样本停止 > sample_timeout(10 ms) => Holdover；> holdover(20 ms) => PtpTimeout 失锁 => 新 epoch。
- 质量：clean 锁定后注入 2000 ppb 漂移且 lock_ppm_limit=100 ppb => 连续 3 坏样本 => Quality 失锁。

## 未测（见 evidence/T19/MANIFEST.md §NOT_RUN）

真实 MOTU/PTP/AoIP 硬件、Linux clock_gettime/rdtsc 板端精度、真实 USB/PTP 双域实流、
2 核重准入新 epoch；本目录只证明离线模型与合成语义。
