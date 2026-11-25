# 校准麦克风登记（MICS）

> 2026-11-17 建立；2026-11-17 17:55 更新（MA1 与 UMIK-1 校准文件均已归档到本目录）。位置与接线为用户确认；序列号/码为设备信息，勿外传。

| 名称 | 型号 | 位置 | 接入 | 校准文件 |
|---|---|---|---|---|
| UMIK-1 | miniDSP UMIK-1（serial 718-7696） | 听音位（固定），离地约 **1.1 m**，离主音箱约 **2 m+**，**90°（麦轴垂直朝上）** | 独立 USB ADC（48k，单声道） | **已有**：90° 与 0° 两份（本场景用 90°） |
| MA1 | Neumann MA 1（serial 6086517654，code 6d7c8e） | 听音区，离地约 **0.8 m**，与 UMIK-1 相距约 **0.8 m**，**离主音箱至少 1 m+**，**90°（垂直朝上）** | MOTU Mic/Line In 1 → capture ch0 | **已有**：normal + inverted（本场景用 90°，normal ADD 语义） |
| ARC | IK Multimedia ARC 校准麦 | 电视柜（固定） | MOTU Mic/Line In 2 → capture ch1 | 无官方校准文件，仅作相对/辅助判断 |

## 测试阶段 vs 生产阶段（重要）

- **测试阶段（当前）**：UMIK-1（听音位）+ MA1（听音位前方）+ ARC（电视柜）三支麦固定摆位（UMIK-1 在听音位；MA1 与 UMIK-1 相距约 80 cm；ARC 在电视柜；UMIK/MA1 与主音箱均至少 1 m+），**仅用于测试便利/多几个点位做相对验证**；除 ARC 外其余都按 **90°（麦轴垂直朝上）** 使用。
- **生产阶段（最终产品流程）**：只使用 **一支 UMIK-1**，由用户依次移动到各个测量点完成单麦多点测量；MA1/ARC 不进入生产校准流程。
- 因此 T67 当前三固定点结果只能作为 pilot/相对验证；正式 MIMO 多点验收（design≥6 + holdout≥3）仍按单 UMIK-1 移动测量的口径执行，不用三麦固定方案冒充。

## UMIK-1（miniDSP，serial 718-7696）

- 校准文件（本目录 calibration-spec/mics/；来自板端 /var/lib/khaen-calib/umik/，2026-11-15 下载，2026-11-17 归档）：
  - umik1_718-7696_90deg.txt，10,215 B，sha256 49482D8773AC5809B07D4A00B027058876AE6F6E3BB821532C483B2CD5588130（板端 7187696_90deg.txt 同 sha）
  - umik1_718-7696_0deg.txt，9,990 B，sha256 2C154823C88560309395983E011E5F4119D09328F5D22141F9CA4B5E07B1805B（板端 7187696_0deg.txt 同 sha）
- 格式：首行 Sens Factor=…dB / SERNO: 7187696 + Auto-generated 90-degree/0-degree calibration file；随后为 Hz<TAB>dB 两列。
- 使用场景：**听音位多声道测量用 90° 文件（麦轴垂直朝天花板）**；0° 仅单只近场。报告记录 mic{model,serial,cal_file,sha256,orientation}。
- 符号语义：miniDSP/REW 惯例加载后用于校正测量；本栈在 T67 接入时做一次**符号自测**（同一测量分别应用 normal/inverted 观察残余趋势）并记录约定，禁止未验证就声称 ±3 dB。

## MA1（Neumann MA 1，serial 6086517654 / code 6d7c8e）

- 来源：Neumann 官方 correction 包（用户 2026-11-17 下载提供）。
- 文件（本目录）：
  - correction_6086517654_6d7c8e.zip，7,640 B，sha256 42A9C00296662FAAB5A94129FDAB336197F98C41CE46F12C9D8CFBF4ACE6DA78
  - ma1_frequency_response_6086517654_6D7C8E.txt，3,585 B，sha256 2E5D8152F2A686BD8FF85DB182163A05B37C72ABE2D7C9E5915533DD2B8E3E62
  - ma1_frequency_response_inverted_6086517654_6D7C8E.txt，3,687 B，sha256 D9EF2AF3896CF2DD49A7CEB0DF174ED828725BEF0AC07EB73DD405CFE0E04DEC
- 格式：两列（Hz, dB），覆盖 9.84–20158.74 Hz；基准 dB relative to 31.3 mV/Pa；官方按 **90° 入射（话筒垂直朝上）** 有效。
- 语义：frequency_response 为 Calibration curve to be ADDED to measured data；inverted 为取反（供减法语义）。本栈默认 **ADD 语义**，并在 T67 做符号自测。
- 备注：MA1 与播放同源时钟（MOTU ch0），用于极性/漂移/近场参考；UMIK 为独立时钟主麦。两者报告均须登记 cal_file+sha256，未加载不得声称 ±3 dB。

## 测量与分析纪律（2026-11-18 事故后新增，强制）

1. **单路录音的 IR 绝对延迟不能换算成距离**：录音的 t=0 是采集启动时刻，不是播放起点；若没有环回/参考通道给出公共时基，`ir.peak_index` / `delay_ms_from_segment_start` 只是提取段内的相对位置，**不得**写成“直达延迟 X ms ≈ Y m”。
2. 只有两种情况可以谈距离：(a) 有 loopback/参考通道或已知播放起点（同一时基）；(b) 同一次采集内比较两路相对延迟（此时偏移抵消）。
3. **必须做几何合理性检查**：若换算出的距离与已知摆位矛盾（例如房间麦被算成 <0.5 m、贴箱/近场），先怀疑时基/峰值选错，不得直接写进结论。
4. 任何“近场/贴箱”结论必须由摆位记录或测距确认，不能只凭 IR 数值推断。