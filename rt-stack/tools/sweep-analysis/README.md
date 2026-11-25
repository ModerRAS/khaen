# sweep-analysis —— 扫频回放分析链（task-229 时基/距离口径修复版）

本目录是从 rt-stack/evidence/board-window-20261118-sweep/scripts 的旧分析脚本派生的**修复版**，
用于重生成 artifacts/khaen-audio/sweep-verification-20261118。旧脚本原样保留在交付目录
SUPERSEDED/old-scripts/（不作为可复跑入口）。

## 文件

| 文件 | 作用 |
|---|---|
| ir_distance.py | IR 时基/距离唯一口径（与 Rust crates/calibration/src/ir_distance.rs 一致）：relative_to_segment / absolute_from_t0 / distance status；无公共 t0 拒绝距离；房间麦 <0.5 m -> SUSPECT |
| selftest_ir_distance.py | 合成 IR 单测（40 项，exit 0/1） |
| analyze_sweep.py | Farina 反卷积 -> IR/FR/RT60/CSD（沿用 T67 pilot 链）；ir.direct_path schema；CLI 时基/麦角色/声速 |
| compare_runs.py | 双路径对比（FR/RT60 + IR 相对偏移 + 距离结论）；不再输出 ir_direct_delay_ms |
| regenerate_artifacts.py | 快照旧产物 -> 重跑分析 -> 映射交付布局 -> 逐字节/语义核对 -> SHA256SUMS |

## 口径（唯一）

- 相对量 ir.direct_path.relative_to_segment：段内峰值位置，只可段内对齐/相对比较；
- 绝对量 = (t_ir_onset - t0)/fs，仅当有公共时基（同一次采集的 loopback/参考通道起点，或
  T61 render frame 与 T62 capture sample 的已知播放起点 t0）；
- 距离 d = 343 m/s * t_direct；无 t0 -> INVALID_NO_COMMON_TIMEBASE，distance_m=null；
- 房间/未知麦 <0.5 m -> SUSPECT_TOO_CLOSE_FOR_ROOM_MIC（先怀疑时基/峰值，不得当近场证据）。

## 常用命令

    # 合成 IR 单测
    python rt-stack/tools/sweep-analysis/selftest_ir_distance.py

    # 只跑分析（默认无公共时基 -> 距离 INVALID）
    python rt-stack/tools/sweep-analysis/analyze_sweep.py \
        --root rt-stack/evidence/board-window-20261118-sweep --out <work>/analysis-new
    python rt-stack/tools/sweep-analysis/compare_runs.py <work>/analysis-new <work>/analysis-new

    # 若确有环回/已知 t0（示例；未在本数据集使用）：
    # --timebase-source loopback --t0-sample <采集域样本> --mic-role room_listening
    # 注意：source!=none 但缺 --t0-sample 时脚本直接报错，不降级输出距离。

    # 重生成交付产物 + 旧产物快照 + SHA256SUMS（会覆盖 artifacts/.../analysis 与 README 不改）
    python rt-stack/tools/sweep-analysis/regenerate_artifacts.py --repo-root projects/khaen-audio-dsp

## 证据

修复报告、diff、测试日志、重生成核对：rt-stack/evidence/cursor-fix-ir-distance/。
