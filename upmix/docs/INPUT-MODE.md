# 输入内容模式：7.1.4 容器里的 2.0 内容自动上混（task-19）

H16 默认固定 Pure/Pass-through，容器可能是 **7.1.4（12ch）**，但节目内容实际只有 **2.0**。
此时不能按容器“直通”，必须由板端做 2.0 → 目标布局（4.1.2/5.3.2）上混；
反之，若 H16 已内部上混（Dolby Upmix / DTS Neural:X）或节目就是多声道，
板端必须**直通**，不能再上混一次（避免双上混叠加）。

## 1. 结论/契约（Lead 2026-11-15 裁决，字段不再改）

### 运行态 /run/khaen/input-content.json（daemon 写，WebUI 只读）

```json
{
  "schema": "khaen-input-content/v1",
  "content_mode": "auto|stereo|multichannel",
  "detected": "unknown|stereo|multichannel",
  "override": null,
  "confidence": 0.92,
  "active_channels": ["FL", "FR", "C"],
  "rms_db": {"FL": -20.1, "FR": -20.0, "C": -22.4, "LFE": -240.0},
  "threshold_db": -50.0,
  "hold_ms": 2500,
  "reason": "secondary-active",
  "h16_internal_upmix": true,
  "container_layout": "h16-7.1.4",
  "detector": "energy",
  "updated_unix": 1794724357
}
```

- `content_mode`：**生效**模式（stereo=走上混，multichannel=走直通，auto=daemon 尚未判定/未运行）。
- `detected`：本轮检测结果；`unknown` = 无信号/迟滞带/未运行。
- `override`：覆盖文件里的值（null=没有覆盖文件；auto/stereo/multichannel 均可能出现）。
- `h16_internal_upmix`：按 detected 推断（multichannel→true，stereo→false，unknown→null）；
  无法直接读 H16 设置，只能按输入内容判断。这是**推断值**，不是 H16 实测状态；H16 的
  Pure/Dolby Upmix/DTS Neural:X 设置与 12ch 容器内容只在离线 `detect --wav` 用合成信号验证过，
  真机 H16 未实测（A-P1-11）。
- `reason`：`fl-fr-only` / `secondary-active` / `no-signal` / `startup-freeze` /
  `override-*` / `daemon-not-running` / `ws-error` 等。
- `detector`：`energy`（自动检测）/ `manual`（被覆盖强制）/ `fallback`（websocket 或 fader 出错）。

### 覆盖文件 /etc/khaen-audio/input-mode.json（WebUI 写，daemon 每轮读）

```json
{"schema": "khaen-input-mode/v1", "mode": "auto|stereo|multichannel", "updated_unix": 1794724357, "updated_by": "webui"}
```

`auto|stereo|multichannel` 为规范值；CLI/daemon 额外接受别名 `force-stereo`、`force-multichannel`
（写入前归一化为 stereo/multichannel）。**优先级：override > detector > auto**。

### 判定规则

1. 只用 **C / SL / SR / height** 的活动判多声道；**LFE 不作判据**：
   - H16 旧固件 2.0 内容会填 L/R + LFE → LFE 有能量仍判 stereo（必须板端上混）；
   - H16 当前固件 7.1.4+Pure+2.0 只填 L/R、其余静音 → 判 stereo。
2. 窗口 2–5 s：daemon 每 500 ms 取一次 CamillaDSP `GetCaptureSignalRms`，
   连续 `hold_ms`（daemon/CLI 默认 2500 ms）满足才切换；进入/退出阈值差 `--hysteresis-db`（默认 6 dB）。
   注意：离线 `detect --wav` 写出的 state 里 `hold_ms` 取内部函数默认 2000，与 daemon 的 2500
   不同（仅元数据差异，判定逻辑不受影响；P2-13）。
3. 启动冻结 2.5 s（默认）：期间保持直通，等 H16/上游格式稳定（H16 待机后可能回 Multi CH）。
4. auto 默认直通：detected 未知/多声道 → 直通；`detected=stereo` → 上混。
5. H16 开 Dolby Upmix / DTS Neural:X 时，它输出的是多声道内容 → 检测为 multichannel → 板端直通，
   自动避免双上混；state 里 `h16_internal_upmix=true`，WebUI 应显示提示。

## 2. 板端实现：双路径单配置 + Aux fader

`khaen-upmix autodetect-config` 生成**一个** CamillaDSP 配置，同时包含两条路径：

```
输入容器（12ch）
  ├─ Mixer paths ─ dest 0..N-1      = 2.0 上混矩阵（用容器 FL/FR 合成，含滤波/延迟）
  │               └ dest N..2N-1    = 多声道直通矩阵（FL←FL、SL←SL、HFL←TFL、SUB←LFE…）
  ├─ Filter（上混路径逐角色 HP/LP/Gain/Delay）
  ├─ Volume Aux1（上混路径总音量；0 dB 开 / -120 dB 静音）
  ├─ Volume Aux2（直通路径总音量；0 dB 开 / -120 dB 静音）
  └─ Mixer sum -> 输出通道（MOTU 22ch 等）
```

- **切换无重启**：daemon 只发 `SetFaderVolume [1, 0|-120]` / `[2, ...]`，交叉渐变由 Volume 滤波器
  `ramp_time`（默认 200 ms）完成。
- `--content-mode stereo|multichannel` 可生成单路径配置（手动固定，不依赖 daemon）。
- 两条路径的输出**只在 fader 正确设置时**互斥（steady state 只有一条路有声）；fader 默认 0 dB
  时两路都开，会叠加。所以启动必须给 CamillaDSP `--gain1=-120 --gain2=0`（默认直通），或让
  daemon 在首个轮询周期内接管；这不是“数学上天然互斥”（P2-12）。

### 默认安全态

CamillaDSP 启动时 Aux 音量默认 0 dB（两条路径都开）。为避免启动瞬间双路径叠加：

- **推荐** CamillaDSP 启动参数带 `--gain1=-120 --gain2=0`（默认直通），或
- daemon 启动后 1 个轮询周期内（≤500 ms）立即写入 fader；
- service 示例在 CamillaDSP 之后启动，会尽快接管；真机部署时请按上面参数启动 CamillaDSP。

## 3. CLI

```bash
# 离线检测容器 WAV（文件后端/测试；输出与运行态同字段 JSON）
khaen-upmix detect --wav /tmp/container714.wav --input-layout h16-7.1.4

# 读运行态（daemon 未跑时返回 detected=unknown / reason=daemon-not-running）
khaen-upmix detect --json

# 覆盖（持久化到 /etc/khaen-audio/input-mode.json；接受 force-* 别名）
khaen-upmix input-mode get
khaen-upmix input-mode set auto|stereo|multichannel

# daemon（读 CamillaDSP websocket；不重启服务、不打开声卡）
khaen-upmix autodetect --input-layout h16-7.1.4 \
  --state /run/khaen/input-content.json --mode-file /etc/khaen-audio/input-mode.json \
  --hold-ms 2500 --startup-freeze-ms 2500

# 离线验证 daemon 判定/迟滞（无 websocket，回放 RMS 序列）
khaen-upmix autodetect --simulate frames.json --dry-run --no-write
```

`--simulate` 文件格式：`{"frames":[{"rms_db":{"FL":-20,"FR":-20,"C":-120}}, ...]}`。

## 4. WebUI 对接（web-manager 实现）

- `GET /api/channelmap/input-mode`：读运行态（只读文件；daemon 未跑时返回 unknown）。
- `PUT /api/channelmap/input-mode {"mode": ...}`：写覆盖文件（接受 force-* 别名），返回新状态。
- 页面：badge 显示 content_mode/detected + 下拉覆盖“自动/强制 2.0/强制多声道” +
  `h16_internal_upmix=true` 时显示“H16 已内部上混，板端直通”提示。
- WebUI 不写 DSP 配置；不改 channelmap v2 schema（v2.1 再正式加字段）。

## 5. 部署/服务

- 安装包包含 /etc/khaen-audio/upmix/khaen-upmix-autodetect.service.example（**默认不启用**）。
- 需要自动切换时：复制为 /etc/systemd/system/khaen-upmix-autodetect.service 后
  `systemctl daemon-reload && systemctl enable --now khaen-upmix-autodetect`。
- 该服务只连 CamillaDSP websocket + 写 state/切 fader，不启动/重启音频服务、不打开声卡。
- 输入容器布局不同（如 h16-5.1.2）时改 `--input-layout`；CamillaDSP 的 capture 通道数需与容器一致
  （autodetect-config 默认取布局 channels，可用 `--capture-channels` 覆盖）。

## 6. 测试（本机 + 板端，纯 stdlib 可复跑）

```bash
python3 tests/gen_container_signals.py --out /tmp/container-signals   # 开发机（numpy）
python3 tests/test_input_mode.py --khaen-upmix ./khaen-upmix \
  --layout-presets /etc/khaen-audio/layout-presets.yml \
  --signals /tmp/container-signals --camilladsp /usr/local/bin/camilladsp
```

覆盖：detect 4 种容器、LFE 单独有能量仍判 stereo、启动冻结、迟滞切换、override 优先级、
别名归一化、双路径文件后端（stereo 模式 == 2ch 上混逐样本一致；pass 模式逐通道映射；
C-only 在 stereo 模式静音；2.0 容器在 pass 模式只有 FL/FR 有能量）。

结果：2026-11-15 **本机 58/58 PASS、板端 OEM v5 58/58 PASS**（RK3576/CamillaDSP 4.1.3 文件后端，
不占声卡）；mock websocket 测试另验证握手/GetCaptureSignalRms/SetFaderVolume/状态文件。
其中板端数据为高度全频化（2026-11-16 裁决）**之前**的旧版；该裁决只改高度声道滤波，
不触及内容检测/双路径切换逻辑，但板端未复跑，标 SUPERSEDED。

## 7. 已知限制

1. H16 设置（Pure / Dolby Upmix / DTS Neural:X）无法直接读取，只能按“输入内容”反推
   `h16_internal_upmix`；H16 待机后模式回 Multi CH 时依赖启动冻结 + 连续检测纠正。
2. 检测是能量型：极安静的多声道段落（只有环境声）可能被判 stereo 而短暂上混；
   迟滞/连续窗口降低误判，但不能完全避免；可用 override 锁定。
3. 双路径配置的 CamillaDSP 默认 fader=0 dB，启动到 daemon 接管前有 ≤1 个轮询周期的双路径叠加风险；
   部署时给 CamillaDSP 加 `--gain1=-120 --gain2=0`（见 §2）。
4. 目前验证到 4.1.2（8ch 输出，MOTU 映射 ch0/1/2/3/4/5/7，ch6 空）与 5.3.2；其它目标布局未测。
5. H16 真机未实测：12ch 容器输入、H16 内部上混状态、待机回 Multi CH 的行为都只有离线合成信号/
   mock websocket 证据（A-P1-11）；上板时需补一次 H16 实测。
6. 双路径配置按 MOTU 22ch 生成，但逻辑上只驱动 4.1.2/5.3.2 槽位；其余通道恒静音（P2-6）。
