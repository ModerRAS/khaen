# khaen daemon HTTP API 合同（工程版 v1，2026-11-26）

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
| GET | `/api/status` | `{daemon:{version,uptime_s,bind}, host_link:{connected,mode,target,age_ms}, format:{...}, input:{...}, output:{...}, plan:{version,graph_id,...}, muted?, safe, reconfigure:{generation,...}, not_wired:[], reasons:{}}`（`ok` 可缺省；拿不到的部分为 null + reasons） |
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
| POST/PUT | `/api/motu/volume` | `{stage:"prepare", writes:[{channel,linear?|db?}]}` | `{ok, engine_token|token, target, write_plan, writes_device:false, ...}`（**不写设备**） |
| POST/PUT | `/api/motu/volume` | `{stage:"commit", engine_token|token}` | `{ok, applied, idempotent, linear, db, channels:[{channel,真值回读}], writes_device:true}` |

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

## 7. 校准（calibration）——实际形状 + 目标增量
现状：uploads/wizard 可用；job 后端当前默认注入 NotWired（写 503 `not_wired`、列表 200 空+reason）；backend-lead 正在接 JobManager/OfflineJobBackend。**下表“实际形状”是前端现在必须适配的**；标【加法】的字段后端接线时一并加，旧字段不删不改义。

| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/calibration/uploads` | multipart WAV（48k/16-24bit/1-2ch） | `{schema, upload_id, sha256, bytes, created}`；格式不符 400 M1 |
| GET | `/api/calibration/jobs` | — | `{schema, available, reason?, engine?, active_job_id?, total, limit, offset, jobs:[{job_id, kind, state, created_at, started_at?, finished_at?, duration_ms?, steps?, apply?, error?}], reasons}` |
| POST | `/api/calibration/jobs` | `{kind:"single_point_offline", inputs:{plan_sha, baseline_upload_id, applied_upload_id, centers_hz:[...], tolerance_db, min_bands}}` | 202 `{job_id, location}`【加法】`ok:true, state` |
| GET | `/api/calibration/jobs/{id}` | — | `{schema, job:{...}, reasons?}`【加法】`ok, result, artifacts` |
| POST | `/api/calibration/jobs/{id}/cancel` | — | `{job_id, status}`（状态字符串 `cancelled`） |
| POST | `/api/calibration/jobs/{id}/apply` | 当前单发 `{}` / `{stage?:...}`；【目标】`{stage:"prepare"|"commit"|"abort", engine_token?}` | prepare 返回草稿+token；commit 生效并记录版本；旧单发语义保留 |
| POST | `/api/calibration/jobs/{id}/rollback` | 同 apply | 同 apply |
| GET | `/api/calibration/jobs/{id}/artifacts/{name}` | — | 二进制/JSON 产物（白名单 + manifest sha 重算，校验失败 400/404） |
| GET/PUT | `/api/calibration/wizard` | 配置对象 | `{schema, config, state, ...}`（实际字段以 daemon 实现为准） |
| POST | `/api/calibration/wizard/apply` | `{...}` | `{job_id, location}`（有 job 时）或配置草稿 |

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

## 11. 裁决记录（2026-11-26，v1.1）
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