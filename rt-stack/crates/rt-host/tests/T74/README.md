# T74 前置：rt-host capture source（file / motu / inferno / aes67）

本目录记录 T74 前置 capture 输入路径的测试口径；**单测本体在 `crates/rt-host/src/main.rs`
的 `#[cfg(test)]` 内**（bin 内部单元测试，不打开真实设备），本 README 只做索引。

两条前置（同一 `main.rs`、互不覆盖）：

- task-196：`capture.source=file`（预渲染 WAV/raw 多声道文件 → 图/plan-arena）；
- task-197：`capture.source=motu|inferno|aes67`（MOTU Toslink ch12/13 / pcm.inferno /
  RAVENNA ALSA，复用 T43 `aoip.rs` 端点/SDP/角色/失流校验；mode 互斥 off|toslink|aes67|dante）。

## 跑法

```powershell
cd rt-stack
cargo test -p tspi-rt-host --offline --bin tspi-rt-host t74       # 33 条（17 file + 16 aoip）
cargo test -p tspi-rt-host --offline --bin tspi-rt-host t74aoip   # 16 条 t74aoip_*（motu/inferno/aes67）
cargo test -p tspi-rt-host --offline                              # 全量（653 passed）
```

## 覆盖范围（fake，本机 Windows）

### file（task-196）

- 正向：2ch 2.0 strict WAV S24_3LE、raw s24_3le/f32le、4.1.2 8ch WAV（ch6 空）→
  plan-arena（n_in=8）→ fake 输出链路；eof=exit 优雅退出 / eof=silence 尾块静音补满；
- 负向：WAV/raw 格式错（非 RIFF、PCM16、截断、多 data）、192k/44.1k 文件、config 192k、
  声道与图/plan 不匹配、4.1.2 ch6 非空、plan n_in 非 2/8、文件缺失、file×loopback 冲突；
- 兼容：默认 synth 路径报告不变、fake ALSA 契约 `hwparams_hash` 不变、真实 block all-ack/retire_ready 语义不变；
- file 输入 → 图 → mock playback sink 写块（3 blocks/384 frames/mute=0，证明进入现有输出链路）；
- F-12：文件解码块循环零堆分配。

### motu / inferno / aes67（task-197）

- mode 互斥：source ↔ mode 一一对应（synth/file=off、motu=toslink、inferno=dante、aes67=aes67），
  6 组错配 + 非法值全部在打开设备前以稳定错误码拒绝；
- 前置：motu 必须 backend=alsa + io_mode=loopback；inferno/aes67 必须 backend=alsa
  且与 loopback 互斥；profile rate（inferno 96k / aes67 48k）；
- 端点（复用 T43）：inferno `InfernoEnv`+`inferno_alsa_endpoint`（pcm.inferno/S32_LE/16ch/
  statime PTPv1）；aes67 `H16FlowConfig`+SDP 逐会话解析+`validate_aes67_capture`+
  `validate_aes67_service`+`Aes67CapturePlan::alsa_endpoint`（RAVENNA 稳定 card id → hw:<card>,0）；
  缺端点/非法 device id/缺 iface/RAVENNA 未找到/SDP 44.1k/daemon 未就绪/PTP 未锁全部拒收；
- 角色映射：strict 2ch(2.0)=FL/FR；8ch(4.1.2)=FL FR SL SR TFL TFR 空 LFE；
  16ch 非逻辑通道（slot 8/9/12/13）显式忽略并计数；3ch → `ERR_ROLES`；
- 失流（T43）：停滞 → 整块静音、同 epoch Resumed；持续失流/端点错误 → `RT_E_DEVICE_LOST`
  fail closed（host epoch owner NOT_WIRED 显式注明）；SafetyMute 块不消费端点数据；
- 报告：`capture` 段给 source/mode/端点/格式/角色/失流计数/NOT_RUN；synth/file 旧字段逐字节不变。

## 相关证据

- `rt-stack/evidence/cursor-fix-capturesrc/`（task-197 MANIFEST、示例配置、fixtures、
  CLI/验证日志与 exit、NOT_RUN；真实 Linux 端点/daemon/PTP/网络与板端 NOT_RUN）；
- `rt-stack/evidence/cursor-fix-fileinput/`（task-196，同口径）。
