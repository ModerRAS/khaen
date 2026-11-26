# khaen daemon HTTP API 合同（工程版 v1.2，2026-11-26）

> 本文件是**前端（webui/）与后端（crates/daemon/）之间的唯一 API 约定**，lead 维护。
> 两边并行开发时必须先按本文件实现；任何 breaking change 必须先改本文件并通知 lead，禁止单边改字段/路径。
> 通用规则：JSON UTF-8、同源；写操作需会话 cookie + `X-KHAEN-CSRF`；**不假成功**（未实现返回 501 unsupported / 503 not_wired + reason，UI 显示原因、不编数据）。

## 1. 通用约定
- 认证：`cookie: khaen_session=<token>`；写请求（POST/PUT/DELETE）必须带 `X-KHAEN-CSRF: <csrf>`（来自登录响应或 `GET /api/auth/session`）。
- 成功响应：HTTP 2xx + JSON；**`ok:true` 是推荐字段但不保证所有端点都有**（/api/status、/api/device、/api/meta 等只读端点历史上没有；新端点应带）。UI 解析成功以 HTTP 2xx + 文档字段为准，不得因缺 `ok` 判失败。
- 错误响应（daemon 本体）：非 2xx + **M1 契约** `{ "error": { "code": "<稳定码>", "message": "<人读>", "details": {...}? } }`。
- 错误响应（经 host_link 转发的 rt-host 错误）：`{ "ok": false, "error": "<稳定码>", "message": "<人读>", "details"?: {...} }`。
- **新增端点一律用 M1 错误体**；前端 client 必须同时兼容上述两种错误体（先把 `error` 当对象取 code/message，否则当字符串用）。
- 稳定错误码与 HTTP 映射：

| code | HTTP | 含义 |
|---|---|---|
| `bad_request` | 400 | 参数缺失/非法 |
| `unauthorized` | 401 | 无/过期会话 |
| `forbidden` | 403 | 已登录但被策略禁止（如 restart_disabled） |
| `not_found` | 404 | 资源/端点不存在 |
| `engine_token_invalid` / `no_base_plan` / `no_history` / `busy` | 409 | 两阶段/历史/并发冲突 |
| `admission_rejected` | 409 | 方案准入拒绝 |
| `unsupported` | 501 | op 存在但未实现（不假成功） |
| `not_wired` | 503 | 后端能力未接线（不假成功） |
| `host_link_unavailable` | 503 | daemon→rt-host 控制通道不可用 |
| `reconfigure_failed` / `ERR_RECONFIGURE_*` | 502 | 数据面热重配失败（含 rolled_back 明细） |
| 其它 | 500 | 内部错误 |

- **两阶段通用语义**：`prepare` 只生成草稿并返回一次性 `engine_token`，不改变运行态；`changed`/`diff`/`current`/`target` 属**可选**回执字段（没有时 UI 显示“服务端未上报”并展示本地 target 预览，不得编造）。`commit {engine_token}` 在块边界生效并返回真实回执；`abort {engine_token}` 丢弃草稿。token 只能消费一次。
- **read-only 永远 200**：`GET /api/status`、`GET /api/channels`、`GET /api/meta` 拿不到数据时返回 `null` + `reasons`，不返回 500、不编数值。
- 新增/修改端点：只允许**加法**（新字段可选、旧字段不删不改义）；breaking change 必须升 `/api/meta` 的 `api_version` 并更新本文件。

## 2. 认证与元信息
| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| GET | `/healthz` | — | `{ok, uptime_s, version}`（version=release 版本） |
| GET | `/api/auth/status` | — | `{ok, initialized, authenticated}` |
| POST | `/api/auth/setup` | `{password}` | `{ok}`（仅首次） |
| POST | `/api/auth/login` | `{password}` | `{ok, csrf, expires_in}` + Set-Cookie |
| GET | `/api/auth/session` | — | `{ok, authenticated, csrf?, expires_in?}` |
| POST | `/api/auth/logout` | — | `{ok}` |
| POST | `/api/auth/password` | `{current, new}` | `{ok}` |
| GET | `/api/meta` | — | `{api_version, web_version, ops:[], surfaces:[], calibration:{wired,reason?}, host_link:{...}, not_wired:[{op,reason}|{surface,reason}]}`（`ok` 可缺省；not_wired 项为 op/surface 二选一形状） |

## 3. 状态与只读视图
| 方法 | 路径 | 成功响应要点 |
|---|---|---|
| GET | `/api/status` | `{daemon:{version,uptime_s,bind}, host_link:{connected,mode,target,age_ms}, format:{...}, input:{...}, output:{...}, plan:{version,graph_id,...}, muted?, safe, reconfigure:{generation,...}, not_wired:[], reasons:{}}`（`ok` 可缺省；拿不到的部分为 null + reasons）。**注意：本响应不含音频模式当前值**（§9.7 G3）；`host_link.mode` 是控制通道传输模式 `socket/file/none`，不是音频模式；rt-host 控制协议 status 里的 `mode` 未投影到 daemon，前端显示“当前模式不可读（无数据源）”。 |
| GET | `/api/device` | 设备监督器快照 `{wired, state, source, ...}`（process 目标时才有 `target`；`ok` 可缺省），不可用给 null+reason |
| GET | `/api/coeff` | 当前已生效方案 `{ok, version, graph_id, ...}` |
| GET | `/api/plan/history` | `{schema:"khaen-plan-history/v1", current:{...}, entries:[{version,graph_id,sha256,committed_at?},...]}`；**加法目标**：补 `ok:true` 与 `versions`（= entries 别名，不改旧字段） |
| POST | `/api/plan/inspect` | 请求 `{plan_path, plan_sha256?}`；响应 `{ok, decoded, admission, graph_id, budget}`（只读试运行） |
| GET | `/api/motu` | `{schema:"khaen-motu/v1", detected, model, card_id, card_index, control_reachable, reason, reason_detail, channel_count, channels:[{channel,name,datastore}], mute, vol:[{channel,linear,db}], state:{...}, read_error, write:{status,single_key,readback,two_phase,batch,max_channels_per_request,reason}, not_wired:[], reasons:{}}`（只读；`ok` 可缺省） |
| GET | `/api/calibration/wizard` | `{ok, config, ...}` |
| GET | `/api/calibration/wizard/state` | `{ok, state}` |
| GET | `/api/system/info` | `{ok, version, uptime_s, host, os, ...}` |
| GET | `/api/system/logs?unit=&lines=` | `{ok, unit, lines, ...}`；不可用给 null+reason（不展示假数据） |
| GET | `/api/system/diagnostics` | **附件下载**：`Content-Disposition: attachment; filename=...`，正文为诊断包（非内联 JSON）；前端用带认证的 blob 下载，不走 JSON 解析 |

## 4. 控制面（两阶段）
| 方法 | 路径 | 请求 | 说明 |
|---|---|---|---|
| POST | `/api/mute` | `{mute:bool}`（旧名 `muted` 作为别名接受） | 静音立即生效，响应带真实回执（mute/muted 均回传） |
| POST | `/api/prepare` | `{plan_path, plan_sha256}`（**必填**，缺省视为 400） | 方案草稿 -> `{ok, engine_token, graph_id, action, budget}` |
| POST | `/api/commit` | `{engine_token}` | 生效 -> `{ok, version, graph_id, applied_block}` |
| POST | `/api/abort` | `{engine_token}` | 丢弃草稿 -> `{ok, aborted}` |
| POST | `/api/surface` | `{surface:"volume"|"routing"|"layout", data:{...}}` | 语义界面第一阶段（返回 engine_token）；mode/format 分别走各自字段见下 |
| POST | `/api/rollback` | `{target?:number}` | 历史旧版本准备成草稿（返回 token），commit 后生效 |
| POST | `/api/format` | 与 format prepare 字段相同（见 §5） | 控制面编译格式两阶段 |
| POST | `/api/device/retry` | — | 触发设备监督器重试 |

`surface` 的 `data` 形状（与 `docs/CONTRACTS.md` B.1.2 一致）：
- volume: `{gains_db:[{lane:number, gain_db:number}]}`
- routing: `{routes:[[lane,dac],...], dac_channels?:number}`
- layout: `{in_channels:number, out_lanes:number, folds:[[src,dst,gain],...]}`
- mode（走 `/api/surface {surface:"mode", data:{mode:"off"|"on"|"aes67"}}`）
- format（走 `/api/surface {surface:"format", data:{sample_rate?,chunksize?,channels?}}` 或 `/api/format`）

### 4.1 MOTU 设备控制（T57 实际实现）
| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| GET | `/api/motu` | — | 见 §3（只读设备快照） |
| POST/PUT | `/api/motu/volume` | `{stage:"prepare", channel: 1|2|"both" 或 channels:[1]|[2]|[1,2], db?:number 或 linear?:number}` | `{ok, engine_token, token, created_at, expires_in_s, target, write_plan, commit_supported, two_phase, writes_device:false, applies_to, target_device}`（**不写设备**；>2 通道 400 `motu_batch_rejected`） |
| POST/PUT | `/api/motu/volume` | `{stage:"commit", engine_token:<token>}` | `{ok, applied, idempotent, engine_token, created_at, linear, db, channels:[{channel,真值回读}], write_plan, writes_device:true, target_device}` |

- 该端点只有 prepare/commit 两阶段，**没有 abort**；未提交草稿由后端 TTL 过期（UI 用本地“取消”丢弃即可，不假定服务端草稿已删）。
- `writes_device` 必须如实：prepare=false、commit 成功=true；失败时不得显示成已写入。

## 5. /api/channels（数据面热重配，两阶段）
- GET `/api/channels`：恒 200；`{ok, schema:"khaen-channels/v1", current:{channels,sample_rate,block_frames,aes67_group,aes67_port,aes67_payload_type,aes67_channels,aes67_map,jitter_frames,device}, device:{name,probe,state}, format:{...}, generation, busy, pending, last_gap_ms, last_result, input:{channels,map}, output:{channels,dac_map,channels_source}, mapping, plan:{graph_id,...}, hot_reconfigure:{wired,reason}, not_wired, reasons}`。
- PUT `/api/channels` 请求 `{stage:"prepare"|"commit"|"abort", ...}`：
  - prepare 可带 `channels`/`sample_rate`/`aes67_group`/`aes67_port`/`aes67_payload_type`/`aes67_channels`/`aes67_map`/`jitter_frames`/`mapping`/`dac_map`；响应 `{ok, changed, diff, current, target, engine_token, token, commit_supported, blockers, admission, graph:{action,reason}, warnings}`。
  - commit `{stage:"commit", engine_token}` 响应 `{ok, applied, changed, generation, gap_ms, format, graph_dropped, rolled_back?, routing:{...}, idempotent?}`；失败按 §1 错误码 + `details.rolled_back`。
  - abort `{stage:"abort", engine_token}` 响应 `{ok, aborted, aborted_token, local:{...}}`。
- **当前值来源**：`mapping` 是 daemon 在 prepare/upload 时记录的方案 routing 镜像（lane->DAC），不是数据面实时回读；方案图被 channels_commit 丢弃或 mirror 未记录时 `mapping=null`，UI 必须显示“当前值未知”并给原因，禁止用旧值冒充当前值。

## 6. 方案文件上传
- POST `/api/plan/upload`：请求体为二进制 plan 与 sha 校验头（见 daemon 实现）；201 首次、200 已存在（dedup）、400 sha 不符。响应 `{ok, stored_path, sha256, dedup}`。

## 7. 校准（calibration）——实际形状（backend 已实现并被测试锁定，本表即 canonical）
现状：uploads/wizard/jobs/artifacts 已可用；生产后端为 JobManager + OfflineJobBackend（`KHAEN_CAL_BACKEND=off` 可关；初始化失败 → not_wired + 真实 reason）。**下表是前端必须适配的实际形状**；标【加法】的字段是已落地的增量。

**字段命名（2026-11-26 裁决 H）**：job 对象用 `id`/`status`（crates/daemon tests 已锁定）；只有创建响应与 cancel 回执用 `job_id`，cancel 同时回 `status` 和 `state` 两字段（同源别名）。前端解析可对 job_id|id、state|status 双接受做防御，但契约测试必须断言本表的实际 canonical 形状。

| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/calibration/uploads` | 原始字节 body（不是 multipart；Content-Type 需受支持，WAV 48k/16-24bit/1-2ch，≤64 MiB） | `{upload_id, sha256, bytes, created}`（201 首次 / 200 已存在去重；**无 schema 字段**）；格式不符 400 M1 |
| GET | `/api/calibration/jobs` | `?limit=&offset=`（1..=200） | `{ok, schema, available, reason?, engine?, active_job_id?, total, limit, offset, jobs:[{id, kind, status, verdict?, created_at, created_unix, started_at?, finished_at?, updated_unix, progress, duration_ms?, error?:{code,message}, reason?}], reasons}` |
| POST | `/api/calibration/jobs` | `{kind:"single_point_offline", inputs:{plan_sha, baseline_upload_id, applied_upload_id, centers_hz:[...], tolerance_db, min_bands}}`（同刻只允许一个 active job，冲突 409 `switch_busy`） | 202 `{ok, job_id, state, location}` |
| GET | `/api/calibration/jobs/{id}` | — | `{ok, schema, job:{id, kind, status, step?, verdict?, created_at, created_unix, started_at?, finished_at?, updated_unix, progress, duration_ms?, engine?, steps:[{name,status,started_at?,finished_at?,sha256?,detail?}], artifacts:[{name,sha256,bytes,content_type,downloadable}], inputs?, result?, error?:{code,message}, reason?, apply_state, applied_version?, apply}, result, artifacts, error, reasons}`（顶层 result/artifacts/error 与 job 内同源【加法】） |
| POST | `/api/calibration/jobs/{id}/cancel` | — | `{ok, job_id, status, state}`（状态字符串 `cancelled`；status/state 同源别名） |
| POST | `/api/calibration/jobs/{id}/apply` | 当前单发 `{}` / `{stage?:...}`；【目标】`{stage:"prepare"|"commit"|"abort", engine_token?}` | prepare 返回草稿+token；commit 生效并记录版本；旧单发语义保留 |
| POST | `/api/calibration/jobs/{id}/rollback` | 同 apply | 同 apply |
| GET | `/api/calibration/jobs/{id}/artifacts/{name}` | — | 二进制/JSON 产物（白名单 + manifest sha 重算，校验失败 400/404） |
| GET/PUT | `/api/calibration/wizard` | 配置对象 | `{schema, config, state, ...}`（实际字段以 daemon 实现为准） |
| POST | `/api/calibration/wizard/apply` | `{job_id?}`（缺省用 last_job_id） | **apply 回执**（与 `/jobs/{id}/apply` 旧单发同形：`{ok, applied:true, job_id, applied_version?, applied_block?, graph_id?, sha256?, snapshot, ack, readback, applied_at, job, reasons}`）；无可用 job 409 `no_applicable_job`，未接线 503 not_wired |

- 状态枚举（canonical）：`queued|running|succeeded|failed|cancelled`（双 l，与实现/现有测试一致；旧文档的 `canceled` 作废）。
- job 审计字段：`job_id, kind, state, created_at/started_at/finished_at`（RFC3339）、`duration_ms, steps, apply, error`；【加法】`created_unix, updated_unix, progress`。
- `measured`/`simulated` 必须如实标记；**模拟结果不得写成实测**。
- 离线 job（`single_point_offline`）不开设备、不出声：JobRequest 对它的 `audible_sweep_authorized` 要求为 false（诚实标注）；真实 UMIK 采集 job 属 T70f-c，NOT_RUN。
- apply/rollback 必须按 `host_link.target` 路由（process=进程内、rt-host=控制通道），不得固定打到 daemon 进程内引擎。
- 错误体：成功 2xx + 上述字段；失败 M1 错误体（见 §1）。

## 8. 系统
| 方法 | 路径 | 请求 | 响应 |
|---|---|---|---|
| POST | `/api/system/password` | `{current, new}` | `{ok}` |
| POST | `/api/system/restart` | `{confirm}`（或 `{target:"rt-host"}`） | 已实现：`KHAEN_ALLOW_RESTART=1` 时经 systemctl 重启 rt-host 并返回回执；未启用返回 403 `restart_disabled`；daemon 自重启 501 `unsupported`；非法 target 501。**UI 必须如实显示 403/501 原因，不得假装已重启** |

## 9. UI 行为约定（前端必须遵守）
1. 每个页面先 `GET /api/meta`（或缓存的 capability）决定展示哪些入口；`not_wired` 的能力显示原因，不渲染可点击的假操作。
2. 写操作一律两阶段：先 prepare（拿到 changed/diff 再让用户确认），再 commit；abort 必须可达。
3. 提交成功以服务端回执为准（graph_id/version/generation/applied_block），不本地伪造；失败展示稳定错误码 + message + rolled_back 等 details。
4. 所有轮询页面（status/motu/channels/jobs）在 401 时跳登录；503/501 显示"未接入/不可用"而不是错误弹窗轰炸。
5. i18n：所有面向用户文案走 zh-CN/en-US 两个 key，同步更新 `i18n:check`。
6. 错误解析：同时兼容 M1 `{error:{code,message,details}}` 与转发 `{ok:false,error:"<code>",message}`；401 统一跳登录；501/503 显示“未实现/未接入 + reason”，不弹重复错误。
7. 当前值不可读时（volume 逐 lane 增益、layout 内容、mode 当前值；routing 用 `/api/channels.mapping` 且需标注来源/降级）：显示“当前值不可读（无数据源）”，不得用默认值/上次值冒充，除非 UI 明示“本地草稿”。
8. 每个新页面/交互必须有对应 mock 契约测试覆盖 prepare/commit/abort/401/503/失败回执；测试断言 path/method/body 与本节及对应章节一致。

## 10. 变更流程
- lead 维护本文件；backend/frontend 子任务发现缺失端点或字段，先在任务中记录“合同缺口”，lead 裁决后更新本文件，再到代码落地。
- 合同测试要求：后端 `crates/daemon/tests/` 断言各端点 shape/错误码；前端 `webui/tests/` 断言所调用的 path/method/body 与本文件一致；两边都跑 fmt/clippy/test。

## 11. 裁决记录（2026-11-26，v1.1–v1.2）
- **A 错误体**：保持 M1 `{error:{code,message,details}}` 为 daemon 规范；host_link 转发错误体照旧；新增端点沿用 M1；不做全站 breaking 改造。
- **B 状态拼写**：canonical=`cancelled`（双 l）。
- **C job 创建**：kind=`single_point_offline`，inputs 为 plan_sha/baseline_upload_id/applied_upload_id/centers_hz/tolerance_db/min_bands；`repeats/drive_level_dbfs/holdout_points` 对离线 job 无意义，不进请求。
- **D plan/history**：保留 `current/entries`，【加法】`ok:true` + `versions` 别名。
- **E 过期项**：mute 字段=`mute`；diagnostics=附件下载；restart 已实现（见 §8）；history 形状按实际。
- **F prepare**：`plan_sha256` 必填（不做缺省自动计算）。
- **G2 surface prepare**：`changed/diff/current/target` 可选，缺省 UI 如实显示“服务端未上报”。
- **G3 当前值**：暂不新增 GET /api/surface 读接口；除 routing 可用 `/api/channels.mapping`（标注来源/降级）外，其余显示“当前值不可读”。可读 surface 状态列为后续独立任务。
- **G5**：以本文件 §7 的“实际形状 + 加法目标”为准（前端先适配实际形状）。
- **G6**：routing 当前值来源与降级口径见 §5 末条与 §9.7。
- **H（v1.2，校准 §7 字段）**：以**后端实际形状**为准修正 §7——job 对象 canonical=`id`/`status`（backend tests 已锁定，不改后端加别名）；创建响应=`{ok, job_id, state, location}`、cancel=`{ok, job_id, status, state}`（status/state 同源别名）；job 详情顶层 result/artifacts/error；uploads 返回 `{upload_id,sha256,bytes,created}`（无 schema）；wizard/apply 返回 apply 回执（无 location）。前端解析保持宽容（job_id|id、state|status）但契约测试断言实际 canonical 形状；本修正不改任何实际行为，不升 api_version。

## 12. 音频通道 profile（输入/输出映射与布局）——v1.2 新增
状态：设计定稿见 `docs/SPEAKER-MAPPING.md`；后端尚未实现。未实现前 GET 返回 null + reasons，写端点返回 503 `not_wired` + reason（不假成功）。新增端点一律 M1 错误体、成功带 `ok:true`。

### 12.1 数据模型（`khaen-audio-profile/v1`，保存在 daemon 状态目录）
```json
{
  "schema": "khaen-audio-profile/v1",
  "device": { "channels": 22 },
  "outputs": [
    { "dac": 0, "enabled": true,  "role": "L", "gain_db": 0.0, "custom_label": null },
    { "dac": 1, "enabled": true,  "role": "R", "gain_db": 0.0, "custom_label": null },
    { "dac": 2, "enabled": false, "role": "None" }
  ],
  "inputs": {
    "sources": [
      { "kind": "aes67",  "enabled": true,  "gain_db": 0.0,
        "channels": [{"channel":0,"role":"L"},{"channel":1,"role":"R"}],
        "preset": "stereo" },
      { "kind": "device", "enabled": true,  "gain_db": 0.0,
        "channels": [{"channel":0,"role":"L"},{"channel":1,"role":"R"}] }
    ]
  },
  "downmix": {
    "center": { "mode": "auto", "gain_db": -3.0 },
    "lfe":    { "mode": "auto", "gain_db": -3.0, "include_heights": false }
  },
  "fanout": { "mode": "unity" }
}
```
- 角色 canonical（19 个）：`L,R,C,Lw,Rw,Ls,Rs,Lb,Rb,LFE,Sub,Tfl,Tfr,Tml,Tmr,Trl,Trr,Custom,None`；地面最多 9、天空最多 6（9.1.6 上限）；**同角色可重复任意多只**；显示名走 i18n（`speakerRole.*`），代码只用 canonical。
- `outputs` 固定 22 行；`enabled=false` 时 role 必须为 `None`；结构性错误只有：启用但角色 None、dac 越界、全部禁用、Custom 无 custom_label。重复角色、缺少角色都**不是错误**（缺少只给 informational warning）。
- `inputs.sources`：`aes67`（RTP 流，当前 2ch）与 `device`（声卡采集通道，**模拟输入和光口都算声卡输入**，按设备实际采集通道列出）。每通道一个角色标签；**两个源可同时启用、各自设成相同的声道布局**，进引擎后按角色**自动求和混音**（每源可选 `gain_db`，默认 0）。AES67 通道顺序**优先取 SDP `channel-order=SMPTE2110.(...)`**（NSP 已发 M/ST/51/71/Uxx），取不到用预设顺序（5.1/7.1 = L,R,C,LFE,Ls,Rs[,Lb,Rb]；天空前→中→后：Tfl,Tfr,Tml,Tmr,Trl,Trr）。
- `downmix` / `fanout` / `preset` 均可选；缺省值见 §12.6，全部可配置（UI 给选项，默认值如下）。

### 12.2 端点
| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| GET | `/api/audio-profile` | — | `{ok, schema, profile, validation:{ok, errors[], warnings[]}, capability:{aes67:{channels, map, channel_order?, channel_order_source:"sdp"|"manual"|"none"}, device_inputs:{available, optical_mode:"spdif"|"adat"|null, sample_rate, channels:[{index, label?}], reason?}}, plan:{lanes, enabled_outputs, dac_channels, graph_id?}}`；read-only 永远 200，不可用字段 null + reasons |
| PUT | `/api/audio-profile` | `{stage:"prepare", profile}` → `{ok, stage:"prepare", profile, validation, diff:{outputs_changed, inputs_changed, downmix_changed, fanout_changed}, token, expires_in_s}`（不碰引擎）；`{stage:"commit", token}` → `{ok, applied:true, saved:true, profile, revision}`；`{stage:"abort", token}` → `{ok, aborted:true}` |
| POST | `/api/audio-profile/plan` | `{}` | `{ok, plan_path, sha256, lane_count, inspect, folds_preview:[{source, channel, role, lane}], downmix_applied:[{kind:"center"|"lfe", gain_db, lanes:[...]}]}`；之后走现有 `/api/prepare` + `/api/commit`（不自动提交） |
| GET | `/api/device/inputs` | — | `{ok, available, optical_mode, sample_rate, channels:[{index,label?,kind:"analog"|"optical"}], source:"motu-alsa", reason?}`（模拟 + 光口全部算声卡输入通道）；拿不到 available:false + reason，不编数值 |
| GET | `/api/channels` | — | 加法字段 `speaker_roles:[{dac, role, enabled}]`、`input_roles:[{source, channel, role}]` |
| POST | `/api/speaker-test` | `{dac:0..21, ms:500..10000, kind:"noise"|"sine", db?:number, audible:true, confirm:true}` | `{ok, dac, ms, kind, level_db, remaining_ms}`；默认 db=-30，范围 -60..-12 |
| POST | `/api/speaker-test/stop` | `{}` | `{ok, stopped:boolean}`（幂等） |
| GET | `/api/status` | — | 加法字段 `speaker_test:{active, dac, remaining_ms, kind}` |

- 错误语义：`bad_request` 400（结构错误/参数越界/未带 `audible`+`confirm`）、`busy` 409（校准或热重配中）、`sound_window_closed` 403（配置了 `KHAEN_SOUND_WINDOW` 且在窗口外）、`not_wired` 503（后端/rt-host 未接线）。

### 12.3 方案生成语义（profile → plan）
1. 启用输出按 canonical 角色顺序排序为 lane 0..N-1（同角色内按 DAC 升序）；`content.in_channels=22`、`out_lanes=N`、`crossover/matrix/protection.lanes=N`、`routing.dac_channels=22`，routes 为 N 条启用行；未启用 DAC **不进路由、不写 CueMix、不做特殊静音**（输出缓冲自然为零）。
2. folds：已启用输入通道按角色 fan-out 到同角色所有 lane（默认 gain 1.0，乘该源 `gain_db`；AES67 与声卡输入同角色自动求和）；缺 C → C 输入额外 fold 到 L/R lane（每 lane 用 `downmix.center.gain_db`）；缺 LFE/Sub → LFE 输入额外 fold 到其它所有启用 lane（`downmix.lfe.gain_db`，`include_heights` 默认 false）——下混只在 folds 增益层，不占矩阵。
3. `fanout.mode`：`unity`（默认，每 lane 1.0）或 `group`（同角色组按 1/√N 归一化，给被动并联音箱用）。
4. 生成预览必须列出真实 fold 列表与下混应用明细；生成后必须 inspect → prepare → commit。

### 12.4 MOTU/CueMix 控制边界（硬规则）
CueMix/控制面写入只允许 4 类用途：①切采样率；②切光口模式 ADAT↔Toslink(S/PDIF)；③整体静音；④整体音量。除此之外不通过 CueMix 写任何东西（不做 per-channel mute/gain/路由，不给未使用 DAC 静音，不给测试音选通道）。只读探测（采样率、光口模式、全局音量/静音）允许，用于能力上报。

### 12.5 UI 约定（“音频映射 / Speaker Layout”页）
1. 输入区两块：AES67、声卡输入；每块可启用，逐通道角色下拉（同一套角色 + 未使用）。AES67 显示顺序来源（SDP / 预设 / 手动）；声卡输入显示只读能力（光口模式、采样率、通道数）。
2. 输出区两步：先选角色与数量（地面 9 / 天空 6 / LFE/Sub，可重复），再逐只分配 DAC；快捷预设 2.0/5.1/7.1/5.1.2/5.1.4/7.1.4/9.1.6 只做填充。
3. 实时校验：重复角色不报错并显示组统计；缺 C / 缺 LFE 显示下混提示与实际增益；显示“将生成 N lane、未使用 DAC 保持无信号”。
4. 每行“识别”按钮：确认框（通道、时长、-30 dBFS 默认），调用 `/api/speaker-test`，出声中可立即停止。
5. 保存草稿（PUT prepare → 展示 diff）→ 应用（POST plan → inspect → prepare/commit）；默认**两步**，用户确认后才生效。
6. 错误/未接线一律显示 null/原因，不编数据；i18n 同步 zh-CN/en-US。

### 12.6 默认值与可选项（用户 2026-11-26 决定：全部给选项 + 默认值）
| 配置 | 默认 | 可选 |
|---|---|---|
| 布局 | 2.0（L=DAC0、R=DAC1，其余禁用） | 快捷预设 2.0/5.1/7.1/5.1.2/5.1.4/7.1.4/9.1.6 + 任意自定义/重复 |
| 输入源 | 只启用 AES67（ch0=L、ch1=R）；声卡输入关闭 | 两块可**同时启用、各设相同布局**，引擎按角色自动求和混音；每源 gain_db 默认 0 |
| AES67 顺序 | SDP channel-order 优先；无则预设顺序 | 手动逐通道改角色 |
| 缺中置下混 | auto，C→L/R 各 -3 dB | auto/none，增益可调 |
| 缺低音炮下混 | auto，LFE→其它音箱各 -3 dB，不含天空 | auto/none，增益可调，可含天空 |
| 同角色 fan-out | unity（每 lane 1.0，用每路 gain_db 微调） | group（1/√N） |
| 声卡输入通道 | 按设备实际采集通道只读列出（Toslink 2ch / ADAT 48k 8ch、96k 4ch + 模拟输入） | 用户逐通道标角色，未用留空 |
| 保存方式 | 两步：保存草稿 + 手动“应用”才重建方案 | 前端可记住上次选择，不默认自动应用 |
| 测试音 | 带限粉噪（500 Hz–4 kHz）1 s、-30 dBFS、20 ms 淡入淡出 | 时长 0.5–3 s（硬上限 10 s）、电平 -40/-30/-20（硬上限 -12）、噪声/正弦 |
| 测试音授权 | `KHAEN_SPEAKER_TEST=on` 才启用；窗口外 403 | 出声窗口配置 `KHAEN_SOUND_WINDOW` |