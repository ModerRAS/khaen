# tspi-audio-web API 文档

> 适用版本：v0.1.0 · 默认监听 `0.0.0.0:80`（局域网直接 `http://<设备IP>/`，无需记端口；mock 为 `127.0.0.1:80`；
> 如 80 被占用可改 `listen` 回退 8088）· 除静态资源外均返回 JSON
> 与代码保持一致：以 `src/routes/` 为准；发现不一致请提 issue。
>
> **现行口径（2026-11-17，F-04/F-44）**：本文件对应的 `tspi-audio-web` 属 **S1 退役栈镜像**——Phase 1 运行镜像不安装它（compat-matrix §F-21），现行 REST/WS/Web 由 `tspi-rt-daemon`（T42/T44/T70）提供；本文仅作历史/迁移参考。
> **CamillaDSP 已退役**（Phase 1 不运行，仅历史/离线 oracle）；旧 web 只保留只读 legacy 状态面（`GET /api/camilladsp` 带 `legacy: true`），**不再提供启停接口**（F-04 已删除 `POST /api/camilladsp/{start,stop,restart}`）。
> **采样率硬约束**：只允许 **44.1/48/88.2/96 kHz**（默认 48 kHz；Dante 96 kHz）；**禁止 176.4/192 kHz**，`PUT /api/audio/format` 提交 192000/176400 → **400**。

## 1. 认证、CSRF 与错误码

| 项目 | 说明 |
| --- | --- |
| 管理密码 | 首次打开 Web 设置（`POST /api/auth/setup`），Argon2id 哈希写 `/etc/tspi-audio-web/auth.json`（0600）；设置后密码登录 |
| token（恢复兜底） | 配置文件 `/etc/tspi-audio-web/config.toml` 的 `token`；保留用于恢复/CLI，`--print-token` 可查，不因设置密码失效 |
| 公开接口 | `GET /healthz`、`GET /api/auth/status`、`POST /api/auth/setup`（仅未设置密码时）、`POST /api/auth/login`、静态资源 |
| 其余接口 | 必须认证：浏览器会话 Cookie `tspi_session`（HttpOnly/SameSite=Strict），或请求头 `X-Auth-Token` / `Authorization: Bearer <token>` |
| CSRF | 会话 Cookie 的写操作（POST/PUT/PATCH/DELETE）必须带 `X-CSRF-Token`（登录响应/`GET /api/auth/session` 返回）；Header token 认证不需要 |
| 会话 | 有效期 `session_ttl_secs`（默认 43200 秒）；内存会话，服务重启后失效 |
| 登录限速 | 同一客户端密码/token 连续失败 5 次锁定 `login_lockout_secs` 秒（默认 60） |

错误响应统一格式：

~~~json
{ "error": { "code": "bad_request", "message": "..." } }
~~~

| HTTP | code | 含义 |
| --- | --- | --- |
| 400 | bad_request | 参数/YAML 语法/越界 |
| 401 | unauthorized | 未认证/会话过期/token 错误 |
| 403 | forbidden | CSRF 失败、未启用的危险操作 |
| 404 | not_found | 资源不存在 |
| 409 | conflict | 模式切换在途/已是当前模式 |
| 429 | too_many_requests | 登录失败过多 |
| 500 | internal_error | CueMix 写入后校验失败并回滚。响应体是 WriteReport（含 `error.code`），不是只有外层 error |
| 503 | service_unavailable | 外部服务不可用（systemd/tspi-mode/CueMix 等离线；CamillaDSP 为已退役 legacy 只读项） |

安全头：`Content-Security-Policy`（仅同源脚本）、`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、`Cache-Control: no-store`。

## 2. 健康检查与认证

### GET /healthz（无需认证）

~~~json
{ "status": "ok", "version": "0.1.0", "mock": true, "uptime_secs": 123, "now_unix": 1700000000 }
~~~

### GET /api/auth/status（无需认证）

~~~json
{ "password_configured": false, "setup_required": true, "token_fallback": true, "mock": false,
  "min_password_len": 8, "recommended_password_len": 10 }
~~~

首次打开页面据此显示“设置管理密码”或登录页；已设置后 `setup_required=false`，不再允许 setup。

### POST /api/auth/setup（无需认证，仅未配置时）

请求：`{ "password": "至少 8 位", "confirm": "再次输入" }`；密码 ≥8 位（推荐 10+）、两次一致。
成功写 `/etc/tspi-audio-web/auth.json`（Argon2id PHC，0600）并立即登录；已配置后返回 **409 conflict**。

响应 200（同时 `Set-Cookie: tspi_session=...`）：同登录响应。

### POST /api/auth/login

请求二选一：`{ "password": "管理密码" }` 或 `{ "token": "配置里的 token" }`（token 为恢复/CLI 兜底，两者都可）。

响应 200（同时 `Set-Cookie: tspi_session=...; HttpOnly; SameSite=Strict`）：

~~~json
{ "ok": true, "csrf_token": "48 位十六进制", "expires_in": 43200, "auth_kind": "session" }
~~~

### GET /api/auth/session

浏览器刷新后恢复 CSRF：

~~~json
{ "authenticated": true, "auth_kind": "session", "csrf_token": "...", "expires_in": 43200 }
~~~

Header token 认证时 `auth_kind=token`、`csrf_token=null`。

### POST /api/auth/logout

注销当前会话，返回 `{ "ok": true }`。

## 3. 总览与系统

### GET /api/overview

返回总览聚合：`system`（CPU/内存/温度/负载/网络/RT）、`services`（实际 `tspi-*.service` 自动发现的状态快照）、`service_notes`（如 Inferno 是 ALSA 插件、无独立服务）、
`camilladsp`（状态/音量/xrun 线索）、`ptp`、`alsa`（卡/PCM）、`mode`、`aoip`、`mock`。

### GET /api/system

~~~json
{ "hostname": "tspi-audio", "kernel": "6.1.x-rt", "os": "Debian 12", "uptime_secs": 100,
  "cpu": { "model": "RK3576", "cores": 8, "usage_percent": 12.5, "load1": 0.1 }, "memory": {}, "realtime": {} }
~~~

### POST /api/system/reboot、/api/system/shutdown

请求体 `{ "confirm": "REBOOT" }` 或 `{ "confirm": "SHUTDOWN" }`。
需配置 `[system].allow_reboot/allow_shutdown` 显式开启，否则 403。

## 4. 运行模式（dante / aes67 / off）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | /api/mode | 当前模式 + 切换状态 + 组件 + 持久化信息 |
| PUT | /api/mode | 触发异步切换，202；在途 409；未启用 403 |
| GET | /api/mode/script-status | 主动执行 tspi-mode status --json 并回传 |

配置：`[mode].enabled`、`[mode].switch_script`（默认 `/usr/local/bin/tspi-mode`）、
`[mode].restore_on_start`（**D4 默认 false**：上电音频 off、不自动恢复；Web 启动不调用脚本）。

`PUT /api/mode` 请求：`{ "mode": "dante" | "aes67" | "off" }`。
优先走 audio-stack 契约：

~~~bash
tspi-mode dante|aes67|off --json
tspi-mode status --json
# 退出码：0 就绪 / 2 参数 / 3 前置 / 4 设备 / 5 PTP 未锁 / 7 DSP 启动失败 /
#         8 自检失败 / 9 已回滚 / 10 另一切换进行中
~~~

脚本缺失时使用内置编排（停止 Camilla → 停 AoIP → 停 PTP → 启目标 PTP/等锁 → 启目标 AoIP →
应用 dante.yml/aes67.yml → 启 Camilla → 复核；失败回滚）。AES67 默认方案 **B1**：
aes67 daemon 内置 PTPv2 slave，不启 ptp4l/phc2sys；Dante 用 Inferno+Statime（PTPv1）；
只有 B2 才走 linuxptp GM。状态持久化在 `[mode].state_file`（`state.json`）。

> 注：以上内置编排是 S1 旧 web 的历史实现（会启停 CamillaDSP）；CamillaDSP 已退役，Phase 1 不再启动它。现行模式/播放切换由 `rt-stack/deploy/`（T71 原子切换）执行，不由旧 web 编排。

## 5. 音频格式 / 缓冲

### GET /api/audio/format

返回当前模式采样率/位深/声道/chunksize/target_level、允许范围、模式默认值、xrun 线索与告警。

允许范围（与页面下拉共用同一白名单，见响应 `limits`）：

- 采样率：**44100 / 48000 / 88200 / 96000 Hz**（44.1/48/88.2/96 kHz；默认 48 kHz，Dante 96 kHz 可用）。
  **禁止 176400 / 192000 Hz**（产品硬约束；MOTU 空闲 192k 根因见 rt-stack 证据 `motu-rate-rootcause`）。
- chunksize：32/64/128/256/512/1024；target_level：0~65536；位深：16/24（一期只读）。

### PUT /api/audio/format

请求（字段可选，至少一个）：

~~~json
{ "sample_rate": 96000, "chunksize": 128, "target_level": 2048 }
~~~

采样率校验：只接受 44100/48000/88200/96000；提交 **176400/192000 返回 400**
（`error.code = "bad_request"`，message 固定为
`采样率 <rate> Hz 已禁用（产品硬约束：禁止 176400/192000 Hz；可选: [44100, 48000, 88200, 96000]）`），
被拒时不会读写 CamillaDSP 配置文件（退役 legacy 只读面）。其他不在白名单的值返回 400「不支持的采样率」。

流程：serde_yaml 校验 → 备份当前配置 → 写 `config_file` → WS `SetConfigJson` →
失败 systemd restart → 再失败回滚。32 帧允许但返回风险提示；位深/声道一期只读。

## 6. CamillaDSP（已退役；legacy 只读对照）

> **CamillaDSP 已退役**（Phase 1 不运行，仅历史/离线 oracle 参考）。旧 web 属 S1 退役栈镜像、Phase 1 不安装；本节仅记录源码保留的 legacy 路由。**F-04（2026-11-17）已删除启停接口** `/api/camilladsp/{start,stop,restart}`（旧前端调用得到 404），不得再用它控制 CamillaDSP。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | /api/camilladsp | 只读状态 + live（版本/state/volume/buffer/xrun）+ 配置列表；响应含 `legacy: true` |
| — | ~~POST /api/camilladsp/start、/stop、/restart~~ | **已删除（F-04）**：返回 404；CamillaDSP 退役后不得作为控制入口 |
| GET | /api/camilladsp/configs | 配置目录列表（名称/大小/修改时间，只读对照） |
| GET | /api/camilladsp/config?name=aes67.yml | 读取 YAML（不存在返回 404，只读对照） |
| POST | /api/camilladsp/configs/switch | `{ "name": "aes67.yml" }`，校验+备份+切换（legacy 文件操作） |
| POST | /api/camilladsp/config/validate | `{ "yaml": "..." }` 语法/结构校验（legacy） |
| POST | /api/camilladsp/volume | `{ "volume_db": -10.0 }`（-150~+50，legacy） |

> 以上配置/音量路由只保留在退役 web 源码内作历史对照；Phase 1 不安装旧 web、不运行 CamillaDSP，不得用这些路由控制运行面（现行控制面见 `rt-stack/`）。

WebSocket 协议核对自 camilladsp 4.1.3 源码（历史记录）：命令为字符串/对象，回复 `{"Cmd":{"result":"Ok","value":...}}`；
注意 4.x **没有 GetXruns** 命令，xrun 由 journalctl 关键词 + StopReason 推断（页面/文档已注明）。

## 7. PTP

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | /api/ptp | 状态（offset/GM/portState/role/source/locked）+ 服务 + 模式说明 |
| GET | /api/ptp/config | 解析 `ptp4l.conf`（[global] masterOnly/priority1） |
| PUT | /api/ptp/config | `{ "master_only": true, "priority1": 5 }`，先备份、重启失败回滚 |
| POST | /api/ptp/start、/stop、/restart | 操作 ptp4l/phc2sys（仅 B2 有意义） |

linuxptp 状态来自 `pmc -u -b 0 "GET TIME_STATUS_NP"` 与 `GET PORT_DATA_SET`；
B1 下由 aes67 daemon 日志/`tspi-mode status` 的 `ptp_lock` 提供。

## 8. AES67 / Inferno

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | /api/aes67、/api/inferno | 服务状态 + 只读配置 + 说明/告警 |
| GET | /api/aes67/logs?lines=100、/api/inferno/logs | journalctl（1~2000 行） |
| GET | /api/aes67/config、/api/inferno/config | 配置只读展示（路径可配） |
| POST | /api/aes67/start、/stop、/restart（Inferno 同） | systemd 操作 |

Inferno 无统一配置文件（上游用 `INFERNO_*` 环境变量 / `/etc/asound.conf`），页面注明。

## 9. ALSA / MOTU

### GET /api/alsa

返回 `cards`（/proc/asound/cards）、`pcm`（含实测通道数）、`hw_params`、`usb_devices`（MOTU 07fd 识别）、`motu_present`、告警。
通道数来自 `/proc/asound/cardX/stream0` 的 Playback/Capture 最大 Channels。

### POST /api/alsa/test-tone

请求 `{ "device": "plughw:1,0", "channel": null }`；使用内嵌 1 kHz/48k/16-bit WAV + `aplay`；
设备名白名单校验，不经 shell。mock 返回未执行命令的结果。

## 9.1 MOTU / CueMix（UltraLite-mk5）

协议与字段对齐 `tools/motu/cuemix_client.py`，说明见 [MOTU-CUEMIX.md](MOTU-CUEMIX.md)。
读接口在设备离线时仍返回 200，`online: false` 与 `reason` 说明原因。写接口在离线时返回 503。
写请求必须带 `"confirm": true`，否则 400。Header token 写操作不需要 CSRF；浏览器会话需要 `X-CSRF-Token`。

### GET /api/motu/status

~~~json
{
  "online": true, "mock": true, "reason": null,
  "discovery": { "uid": "UL5LFE7E6E", "ip": "169.254.111.126", "version": "2.1.2+3352", "iface": "enx0001f2fe7e6f" },
  "device": { "model": "UL5L", "name": "UltraLite-mk5", "sample_rate": 48000, "mac": "00:01:f2:fe:7e:6e" },
  "last_read_unix": 1763260000,
  "main_trim": 28, "main_db": -28.0,
  "direct_count": 7, "direct": [],
  "phantom": [{ "index": 0, "name": "Mic/Line 1", "on": false }],
  "last_error": null, "latest_snapshot_id": null,
  "message_count": 0, "cache_age_ms": 0
}
~~~

`main_trim` 是 `kMainTrim` 的字节，`main_db = -main_trim`。`direct` 是推子大于 0 且未静音的物理直通（含模拟别名 `in7→out7` / `in8→out8`）。幻象电源只读。

### GET /api/motu/snapshot

结构化摘要：`device`、`buses`（14）、`inputs`（32）、`direct`、`nonzero`（推子非 0 的交叉点）。
`?raw=1`（也接受 `true`/`yes`）附加 datastore JSON；默认不带，`raw` 为 null、`raw_omitted` 为 false。请求了 raw 且序列化超过 512 KiB 时省略正文，`raw_omitted` 为 true，并在 `reason` 说明。

### POST /api/motu/monitor

`{ "trim_db": 28, "confirm": true }`。`trim_db` 是 0..=100 的整数，写入 `kMainTrim` 字节 n，含义是 −n dB。范围依据 CueMix 前端 `kMainVolCtrlInfo {min:100, max:0}`。

### POST /api/motu/direct-mute

`{ "mute": true, "confirm": true }`：final insert。候选集是 14 条总线 × 20 个直通源 = 280 格 `kiMixFader`，线上只写与当前值不同的格子，主机输入 18..29 不写。
`{ "mute": false, "confirm": true }`：用最近一份快照把这 280 格推子写回去，不改 `kMainTrim`，也不改主机输入。

### POST /api/motu/restore

`{ "snapshot_id": "可选", "confirm": true }`。缺省用最近一份。恢复键与 client.py 的 `MIXER_KEYS` 相同，**不含** `kMainTrim`。找不到指定 id 返回 404；还没有任何快照返回 400。

写成功 200，失败回滚 500。两者都是 WriteReport：

~~~json
{
  "ok": true, "changed": 7, "written": 7, "verified": true,
  "rollback": false, "rollback_verified": false,
  "snapshot_id": "20261116T030405006Z-0001",
  "candidates": 280, "host_inputs_untouched": true,
  "message": "全部静音：已写入 7 条差异并读回校验通过",
  "mismatches": []
}
~~~

校验不一致时 `ok: false`、`rollback: true`，`mismatches[]` 带 `key/index/expected/actual`，并带 `error: { "code": "internal_error", "message": "..." }`。

真机快照目录默认 `/var/lib/tspi-audio-web/motu-snapshots/`，只留最近 20 份。mock 快照只在内存里。

## 10. 声道分配 / 校准 / remap（channelmap v2）

契约以 audio-stack 的 tspi-channelmap 为准（Python 工具 + /etc/tspi-audio/layout-presets.yml）：

- active：`/etc/tspi-audio/channelmap.yml`（schema `tspi-channelmap/v2`，首次开机没有该文件）
- 预设：`/etc/tspi-audio/layout-presets.yml`（schema `tspi-layout-presets/v2`，只读；含 input_layouts / output_layouts / remap_presets）。
  Web 内嵌副本与之逐项一致（sha256 `48561092…`）；**4.1.2 = FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7（ch6 空，MOTU Line Out 1-6/8）**，
  `tests/presets_drift.rs` 在 cargo test 中比较两侧 role→outputs/输入映射/remap preset，防漂移。**域约定（角色 vs 槽位 vs 设备 22ch）见 §10.1**。
- profiles：`/etc/tspi-audio/profiles/*.yml`；渲染：`/run/tspi/camilladsp.yml`；last-good：`/var/lib/tspi/channelmap/`
- CLI：`tspi-channelmap probe|validate|print|render|apply|rollback|profiles|save-profile|load-profile|export|import`
- 退出码：0 ok；2 schema/用法；3 取值/校验；4 渲染/camilladsp --check；5 apply/重启；9 已回滚 last-good
- 严格校验：v2 拒绝未知字段；`output.roles[].sources` 与 `remap.preset/routes` 不能同时非空（sources 是最终矩阵，remap 只用于生成）

### 10.1 声道域约定（角色 / 槽位 / 设备 22ch，C-06）

同一份 channelmap v2 里有三个不同的“声道号”域，**不能互相混用**（口径来源：板端 `audio/overlay/etc/tspi-audio/layout-presets.yml` +
MOTU UltraLite-mk5 22ch 实测）：

| 域 | 字段/位置 | 含义 | 4.1.2 取值 |
| --- | --- | --- | --- |
| 角色（role） | `output.roles[].id`、`sources[].source` | 布局角色名（FL/FR/SL/SR/HFL/HFR/SUB1…） | 7 个角色 |
| **布局槽位**（slot） | `output.roles[].outputs[]`（0-based，可多通道 fan-out） | 布局内部输出槽位号；**Web API 里的 `outputs` 全部是槽位，不是 MOTU 设备通道号** | `0,1,2,3,4,5,7`（共 8 槽，ch6 空闲） |
| 设备播放通道 | `output.channels` / probe `playback.channels` / apply `--playback-channels` | 播放设备实际通道数（UltraLite-mk5 恒 22ch）；`output.channels` 只是无 probe 时的 fallback | 22 |
| 物理输出（1-based） | 文档/报告里的 “Line Out N” | 0-based 槽位 +1（MOTU Line Out 1-6,8 ↔ 槽位 0-5,7） | Line Out 1-6,8；SUB1=Line Out 8 |

- **4.1.2**：7 个角色占 **8 个槽位**（0..7；HFL/HFR=槽位4/5=Line Out5/6，SUB1=槽位7=Line Out8，ch6 不用）；设备仍是 22ch。
- **5.3.2**：10 个角色占 10 个槽位（0..9）；设备仍是 22ch。
- 输入侧 `input_layouts[].roles[].channel` 是该角色在输入 flow 内的 0-based 通道；多 flow（streams）时全局通道 = 前序 flow 通道数之和 + channel，
  与输出槽位无关。
- 渲染：槽位号即 CamillaDSP 混音器/滤波器内部通道号（0-based），`tspi-channelmap render` 生成的 `mixers/...` 与 `pipeline[].channels` 用同一槽位。
- 因此：`GET /api/channelmap` 的 `presets`/`document`、`POST /api/channelmap/draft` 生成的 `roles`、`profiles/*.yml` 的 `outputs`
  都是**槽位域**；只有 `probe.playback.channels`、`device`、apply 的 `--playback-channels` 是**设备域**。
- 反例（已修复，2026-11-16）：旧 4.1.2 把 SUB1 写到槽位 4、HFL/HFR 写到 5/6，会把 20–300 Hz 打进高度箱且 SUB 静音
  （正确：HFL=4、HFR=5、SUB1=7；见 `tests/presets_drift.rs` 与 `channelmap.rs` 单测）。

模型（WebUI 保存前会物化为显式 roles+sources；下方示例的 `outputs` 是槽位，`output.channels` 是设备通道数）：

~~~yaml
schema: tspi-channelmap/v2
version: 2
profile: { name: "5.3.2-h16-714", description: "..." }
input:  { layout: "h16-7.1.4", channels: auto, presets: {}, probe_order: null }
output:
  device: auto
  channels: 22                 # 设备播放通道数（MOTU 恒 22ch）；不是布局槽位数。实测优先，profile 值仅 fallback
  layout: "5.3.2"
  roles:
    - id: FL
      label: Front L
      enabled: true
      outputs: [0]             # 布局槽位（0-based，可多通道 fan-out），不是设备/Line Out 编号
      sources:                 # 输入源（多源求和；来源是 input layout 角色 id）
        - { source: FL, gain_db: 0, inverted: false, mute: false }
    - id: SUB1
      outputs: [5]             # 本例 layout=5.3.2 的槽位 5；4.1.2 的 SUB1=槽位 7（见 §10.1）
      sources: [{ source: LFE, gain_db: 0 }]
      highpass: { freq: 20, order: 4, type: LinkwitzRiley }
      lowpass:  { freq: 80, order: 4, type: LinkwitzRiley }
      peq:
        - { freq: 1000, q: 1.0, gain_db: 0 }                       # type 缺省 = peaking
        - { freq: 80,   q: 0.7, gain_db: 2.5, type: low_shelf }    # v1.1；shelf 的 q = RBJ slope S
        - { freq: 4000, q: 0.7, gain_db: -2,  type: high_shelf }
      fir: { path: /etc/tspi-audio/coeffs/sub1_48k.wav, samplerate: 48000 }
remap: { preset: null, routes: [], drop_unused: true, silence_unmapped: true }
bass_management:
  enabled: false
  lfe_role: LFE
  lfe_to: []                   # 输出角色 id 列表
  redirect: { enabled: true, sources: [], targets: [], gain_db: 0 }
  mains_highpass_hz: 80, mains_highpass_order: 4
  sub_lowpass_hz: 80, sub_lowpass_order: 4, sub_gain_db: 0
~~~

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | /api/channelmap | 视图：document/effective_roles/input_roles/validation/presets/probe/profiles/fir_files/mode_off |
| POST | /api/channelmap/draft | `{ input_layout, output_layout, profile_name?, capture_channels?, playback_channels? }` 生成显式草稿 |
| POST | /api/channelmap/validate | 校验文档：委托 `tspi-channelmap validate --json`（板端唯一权威）；CLI 不可用/输出无法解析返回 503，**不用本地规则放行**。mock 模式用与板端对齐的本地规则 |
| PUT | /api/channelmap | 保存 active（物化 roles+sources；force=true 允许存草稿） |
| POST | /api/channelmap/apply | 保存→校验→tspi-channelmap apply --mode <mode> --capture-channels N --playback-channels M；off 只校验/渲染/持久化，失败（如 exit 9）返回回滚信息 |
| POST | /api/channelmap/rollback | tspi-channelmap rollback |
| GET | /api/channelmap/render | 渲染结果文本：真机委托 `tspi-channelmap render`（CLI 不可用返回 503，不静默读旧文件）；mock 读 /run/tspi/camilladsp.yml |
| GET/POST | /api/channelmap/profiles | profile 列表 / 保存（`{name, document?}`） |
| POST | /api/channelmap/profiles/load /delete | `{name}`；load 覆盖 active（备份旧文件），不自动 apply |
| GET | /api/channelmap/export?name= | 导出 YAML |
| POST | /api/channelmap/import | `{yaml, name?}`；name 为空写 active |
| GET/POST | /api/channelmap/fir | FIR 列表 / 上传（base64，≤16MiB，解析采样率/taps/群延迟） |
| POST | /api/channelmap/test-tone | `{role?, output_channel?, kind:"tone"|"sweep", duration_ms?}` |
| GET/PUT | /api/channelmap/input-mode | 输入内容模式（auto/2.0/multichannel）：GET 读 /run/tspi/input-content.json + 覆盖文件；PUT `{mode}` 写 /etc/tspi-audio/input-mode.json（兼容 force-stereo/force-multichannel 别名，CLI 存在时优先） |

校验/行为要点：

- 通道数：`input.channels`/`output.channels` 可为 auto；apply/validate 优先 probe 的 capture/playback 通道数，未知时要求用户选择；任何 role.outputs ≥ 实测输出通道、输入 role 全局通道 ≥ 实测输入通道 → 阻止 apply。
- 输出角色 enabled 时 outputs 不能为空且不能互斥冲突；gain -60..+12 dB、delay 0..200 ms、polarity normal|invert、HP/LP freq 20..fs/2（最严按 48k）、阶数 2/4/6/8、PEQ freq/q/gain 范围、最多 32 段。
- **PEQ v1.1（2026-11-16 裁决 5）**：`peq[].type` 可省略（= `peaking`）或取 `peaking|low_shelf|high_shelf`；其它 type 拒绝（板端 schema error rc=2）。
  **peaking** 的 `q` ∈ `[0.1, 10]`。**shelf 的 q 是 RBJ shelf slope S**（不是 Q 型），必须在 `[0.1, 1.0]`
  （CamillaDSP `slope=12*S` 上限 12 dB/oct；q>1 为 value error，rc=3，不产文件）。
  板端按 `Lowshelf|Highshelf{freq, slope=12*q, gain}` 渲染，与 producer 的 shelf 模型（`peq.rs` 的 S 公式）系数一致。
  转换/保存**不得丢 `type`**（丢 = 静默降级成 Peaking，响应差可达 ~3 dB）。
- 板端 `tspi-channelmap`（Python）是校验/渲染/apply 的唯一权威：Web 只委托 + 展示；CLI 缺失/不可用时返回 503（"tspi-channelmap 不可用，无法校验/渲染"），
  不以本地规则放行。mock（`TSPI_MOCK=1`）模式用与板端对齐的本地规则（type 白名单、peaking q [0.1,10]、shelf q [0.1,1.0]），仅用于测试/演示。
- FIR：path 可为相对（相对 profile 目录）或绝对；samplerate 必填或从 sidecar/文件名解析；与当前模式 48k/96k 不一致时校验失败（按模式分 profile）；taps >4096 实验性、>8192 强警告（v2.1 才支持 --force）。
- bass_management enabled 时：lfe_to/redirect.targets 引用的 SUB 角色 sources 必须为空（渲染器自动生成 LFE+redirect 并接管 mains HP）。
- H16：输入布局仅 5.1/7.1/5.1.2/5.1.4/7.1.2/7.1.4；7.1.4 = flow0 1-8 + flow1 11/12/15/16（12 路），5.1.2 = 8 路；SDP 只给每流通道数。
- v2.1（ART-like 全 MIMO per-route FIR）字段草案：sources[].fir{path,samplerate,taps,band_limit,sparse}、roles[].limiter{threshold_db,release_ms}/max_gain_db、print --json 的 estimated{gflops_direct,gflops_fft,group_delay_ms,paths,taps_max}；WebUI 已留占位，待 benchmark 后实现矩阵页。
- 输入内容模式（不改 channelmap v2 schema）：运行态 /run/tspi/input-content.json（schema tspi-input-content/v1，字段 content_mode/detected/override/confidence/active_channels/rms_db/threshold_db/hold_ms/reason/h16_internal_upmix/container_layout/detector/updated_unix）；覆盖文件 /etc/tspi-audio/input-mode.json {schema,mode}（mode=auto|stereo|multichannel）。优先级 override > detector > auto；auto 默认直通，detected=stereo 走 upmix；h16_internal_upmix=true 或 detected=multichannel 禁止二次上混。

## 11. 静态资源

| 路径 | 说明 |
| --- | --- |
| /、/index.html | 管理页面（内嵌，中文，10 页，含 MOTU / CueMix） |
| /style.css、/app.js、/favicon.svg、/favicon.ico、/test-tone.wav | 静态资源（内嵌，无外部 CDN） |

## 12. mock 模式与失败注入

`TSPI_MOCK=1 cargo run` 后所有外部命令走内存 mock，默认 token `mock-token`：

| 环境变量 | 作用 |
| --- | --- |
| TSPI_MOCK_FAIL_START、TSPI_MOCK_FAIL_RESTART | 指定 unit 启动/重启失败（逗号分隔） |
| TSPI_MOCK_FAIL_APPLY=1 | CamillaDSP 应用失败 → 回滚 |
| TSPI_MOCK_STEP_DELAY_MS=150 | 模式切换每步延迟 |
| TSPI_MOCK_PERSISTED_MODE=dante | 预置持久化模式（展示用） |
| TSPI_MOCK_CHANNELMAP_PRESET=5.3.2 | 预置 channelmap 文档（跳过首启向导） |
| TSPI_MOCK_CHANNELMAP_FAIL_APPLY=1 | channelmap apply 失败注入（验证回滚提示） |
| TSPI_MOCK_MOTU_APPLY_FAILS=1 | CueMix 写入不落地，返回 500 且 `rollback: true`（状态不变） |

## 13. 接口完整路径清单

| 方法 | 路径 | 认证 | 说明 |
| --- | --- | --- | --- |
| GET | /healthz | 否 | 健康检查 |
| POST | /api/auth/login | 否 | 登录 |
| GET | /api/auth/session | 是 | 会话/CSRF |
| POST | /api/auth/logout | 是 | 注销 |
| GET | /api/overview | 是 | 总览 |
| GET | /api/system | 是 | 系统信息 |
| POST | /api/system/reboot、/api/system/shutdown | 是 | 电源（需确认短语） |
| GET/PUT | /api/mode | 是 | 运行模式 |
| GET | /api/mode/script-status | 是 | tspi-mode 状态 |
| GET/PUT | /api/audio/format | 是 | 音频格式/缓冲 |
| GET | /api/camilladsp | 是 | 状态（legacy 只读，`legacy:true`） |
| — | ~~/api/camilladsp/start、/stop、/restart~~ | — | **已删除（F-04）**：返回 404 |
| GET | /api/camilladsp/configs、/api/camilladsp/config | 是 | 配置列表/读取（legacy） |
| POST | /api/camilladsp/configs/switch、/api/camilladsp/config/validate | 是 | 切换/校验（legacy 文件操作） |
| POST | /api/camilladsp/volume | 是 | 音量（legacy） |
| GET | /api/ptp、/api/ptp/config | 是 | PTP 状态/配置 |
| PUT | /api/ptp/config | 是 | 改 masterOnly/priority1 |
| POST | /api/ptp/start、/stop、/restart | 是 | linuxptp 服务 |
| GET | /api/aes67、/api/aes67/logs、/api/aes67/config | 是 | AES67 |
| POST | /api/aes67/start、/stop、/restart | 是 | AES67 服务 |
| GET | /api/inferno、/api/inferno/logs、/api/inferno/config | 是 | Inferno |
| POST | /api/inferno/start、/stop、/restart | 是 | Inferno 服务 |
| GET | /api/alsa | 是 | 声卡/PCM/USB |
| POST | /api/alsa/test-tone | 是 | 测试音 |
| GET | /api/motu/status | 是 | CueMix 在线状态、Main、直通、幻象 |
| GET | /api/motu/snapshot | 是 | CueMix 结构化快照（`?raw=1` 才带原始 datastore） |
| POST | /api/motu/monitor | 是 | 写 kMainTrim（须 confirm） |
| POST | /api/motu/direct-mute | 是 | final insert / 从快照恢复直通（须 confirm） |
| POST | /api/motu/restore | 是 | 按 MIXER_KEYS 恢复快照（须 confirm） |
| GET | /api/channelmap | 是 | channelmap 文档/检测/校验 |
| POST | /api/channelmap/draft、/validate、/apply、/rollback | 是 | 草稿/校验/应用/回滚 |
| PUT | /api/channelmap | 是 | 保存 |
| GET/POST | /api/channelmap/profiles | 是 | profile 列表/保存 |
| POST | /api/channelmap/profiles/load、/delete | 是 | profile 加载/删除 |
| GET | /api/channelmap/export | 是 | 导出 YAML |
| POST | /api/channelmap/import | 是 | 导入 YAML |
| GET/POST | /api/channelmap/fir | 是 | FIR 列表/上传 |
| POST | /api/channelmap/test-tone | 是 | 按角色/通道测试音 |
| GET | /、/index.html、/style.css、/app.js、/favicon.svg、/favicon.ico、/test-tone.wav | 否 | 静态资源 |
