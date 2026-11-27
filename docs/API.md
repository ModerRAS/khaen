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
| GET | `/api/auth/status` | — | `{configured, authenticated}`（匿名；**无 `ok`/`initialized` 字段**） |
| POST | `/api/auth/setup` | `{password, confirm}`（≥8 字符、两次一致） | **201** `{csrf, expires_in}` + `Set-Cookie: khaen_session`；已配置 409 `already_configured`，弱密码 400 `weak_password`，不一致 400 `password_mismatch` |
| POST | `/api/auth/login` | `{password}` | **200** `{csrf, expires_in}` + `Set-Cookie`；未配置 409 `not_configured`，密码错 401 `invalid_credentials`，同源 60s 内失败 5 次后 429 `rate_limited` |
| GET | `/api/auth/session` | 需会话 | `{authenticated:true, csrf, expires_at}`（RFC3339）；无会话 401 |
| POST | `/api/auth/logout` | — | **200** `{ok:true}` + 清 Cookie（幂等） |
| POST | `/api/auth/password` | `{old, new}` | **200** `{csrf, expires_in}` + 新 Set-Cookie（旧会话全部失效） |
| GET | `/api/meta` | — | `{ok?, api_version:number, web_version:string, ops:string[], surfaces:string[], calibration:{wired:boolean, reason:string|null}, host_link:{...}, not_wired:[{op?,reason}|{surface?,reason}]}`；`not_wired` 项 op/surface 二选一；`surfaces` 实际为字符串数组（也兼容 `{surface:bool}` 旧形态）；前端把 `api_version` 归一化成 string。 |

## 3. 状态与只读视图
| 方法 | 路径 | 成功响应要点 |
|---|---|---|
| GET | `/api/status` | `{daemon:{version,uptime_s,bind}, host_link:{connected,mode,target,age_ms}, format:{...}, input:{...}, output:{...}, plan:{version,graph_id,...}, mode?, muted?, safe, reconfigure:{generation,...}, not_wired:[], reasons:{}}`（`ok` 可缺省；拿不到的部分为 null + reasons）。**`mode` 为【加法】字段**：来自 rt-host 控制 status 顶层 `mode`（如 `aes67`），仅 socket 主路径有真值；file 回退/process 目标/链路不可用/旧 socket 缺字段时 `null` + 非空 `reasons.mode`，不编造。**`host_link.mode` 是控制通道传输模式 `socket/file/none`，与音频模式不是一回事**。**`readback` 为【加法】字段**：控制面 `base_plan`（启动时加载或最近一次 commit 的方案）的只读语义快照（volume/layout/routing 的“当前值”数据源；commit 后在下一个块边界生效，`switch_pending` 另行如实上报），形状 `{available,source:"base_plan",sample_rate,block_size,content:{in_channels,out_lanes,folds:[{src,dst,gain}]},crossover:{lanes,filters:[{lane,taps}]},matrix:{rows,cols,decimation,interpolation,taps},protection:{lanes,entries:[{lane,delay_samples,polarity,gain_db,peq_bands}]},routing:{dac_channels,routes:[{lane,dac}]}}`；**只给控制面语义，不复制 FIR/矩阵系数**。仅 socket 主路径有真值；file 回退/process 目标/链路不可用/旧 rt-host 为 `null` + `reasons.readback`，不拿默认布局或缓存冒充当前值。**`format.encoding` 已补源**：socket 主路径取 rt-host control status `format.encoding`（当前 `S24_3LE`，即数据面 ALSA 输出编码）；file/process/旧 rt-host 仍为 `null` + `reasons.format.encoding`，不把编码写死成本地默认。 |
| GET | `/api/device` | 设备监督器快照 `{wired, state, source, ...}`（process 目标时才有 `target`；`ok` 可缺省），不可用给 null+reason |
| GET | `/api/coeff` | 当前已生效方案 `{ok, version, graph_id, ...}` |
| GET | `/api/plan/history` | `{ok, schema:"khaen-plan-history/v1", current:{...}, entries:[{version,graph_id,sha256,committed_at?},...], versions（= entries 别名）}`；v0.1.11 真 daemon 另带 `source`（daemon-process/rt-host/file/none）、`pagination`、`reasons`（缺失字段原因） |
| POST | `/api/plan/inspect` | 请求 `{plan_path, plan_sha256?}`；响应 `{ok, decoded, admission, graph_id, budget}`（只读试运行） |
| GET | `/api/motu` | `{schema:"khaen-motu/v1", detected, model, card_id, card_index, control_reachable, reason, reason_detail, channel_count, channels:[{channel,name,datastore}], mute, vol:[{channel,linear,db}], state:{...}, read_error, write:{status,single_key,readback,two_phase,batch,max_channels_per_request,reason}, not_wired:[], reasons:{}, cache:{age_ms,stale,last_error}}`（只读；`ok` 可缺省） | **`cache` 为【加法】字段**：有近期请求时 daemon 后台约 2 秒刷新一次，约 30 秒无请求后停止；瞬时读失败保留上一份成功快照并置 `stale=true`/`last_error`，不把已知字段清成 null。
| GET | `/api/calibration/wizard` | `{ok, schema, config, defaults, saved, updated_at, reasons}`（PUT 同形 + `set_fields`；详见 §7） |
| GET | `/api/calibration/wizard/state` | `{ok, schema, current_step, steps:[{step,key,status,reason,task}], active_job_id, last_job_id, measurement, snapshot, updated_at, reasons}`（8 步 gate 由 daemon 判定；详见 §7） |
| GET | `/api/system/info` | `{ok, version, uptime_s, host, os, ...}` |
| GET | `/api/system/logs?unit=&lines=` | `unit` ∈ `daemon`/`rt-host`（也接受 `rt_host`；其它值 400 `invalid_request`，details 给 `allowed`）；`lines` ∈ 1..=500（默认 200）。200 → `{ok, schema:"khaen-system-logs/v1", unit, unit_name, requested_lines, lines:[...], truncated, source:"journalctl", reason:null}`；journalctl 不可用 → 503 `logs_unavailable` + details{reason,unit}（不展示假数据） |
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
- **`/api/mute` 的 MOTU 副作用（target=process 且检测到 MOTU；实际形状）**：
  - 成功 200：原引擎回执 + `muted` 别名 + 【加法】`motu:{detected, model, control_reachable, reason, reason_detail, side_effect:true, applied, write}`；设备不可用时 `applied:false` + reason（**仍是 200，不报错**，Windows/无设备是常态）；写入并回读成功 `applied:true` + `write:{key,index,before,wrote,readback,changed,frame_sent,verified}`。
  - 设备写入/回读失败：502（内部错误 500）+ **非 M1 外壳** `{ok:false, error:{code, message, details:{phase:"motu_mute", motu:{detected,model,card_id,card_index,control_reachable,reason,reason_detail}, motu_error:{code,message,retryable}}}, control:<引擎静音回执>}`——"引擎已静音"与"设备静音失败"分开上报，不假成功；前端必须展示 `details.motu_error` 且提示引擎侧已生效。
  - target=rt-host：行为完全不变（无 `motu` 字段）。

## 5. /api/channels（数据面热重配，两阶段）
| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| GET | `/api/channels` | — | 恒 200 只读快照（字段见下） |
| PUT | `/api/channels` | `{stage:"prepare"|"commit"|"abort", ...}`（见下） | 两阶段回执（见下） |

- GET `/api/channels`：恒 200；`{ok, schema:"khaen-channels/v1", target, runtime_mode, current:{channels,sample_rate,block_frames,aes67_group,aes67_port,aes67_payload_type,aes67_channels,aes67_map,jitter_frames,device}, device:{name,probe,state}, format:{...}, generation, busy, pending, last_gap_ms, last_result, applied:{...}, input:{channels,map}, output:{channels,dac_map,channels_source}, mapping, plan:{graph_id,...}, hot_reconfigure:{wired,reason}, not_wired, reasons}`（`target`/`runtime_mode` 是 v0.1.11 真 daemon 实际字段：控制目标与 process/rt-host 运行时模式）。
- PUT `/api/channels` 请求 `{stage:"prepare"|"commit"|"abort", ...}`：
  - prepare 可带 `channels`/`sample_rate`/`aes67_group`/`aes67_port`/`aes67_payload_type`/`aes67_channels`/`aes67_map`/`jitter_frames`/`mapping`/`dac_map`；响应 `{ok, changed, diff, current, target, engine_token, token, commit_supported, blockers, admission, graph:{action,reason}, warnings}`。
  - commit `{stage:"commit", engine_token}` 响应 `{ok, applied, changed, generation, gap_ms, format, graph_dropped, rolled_back?, routing:{...}, idempotent?}`；失败按 §1 错误码 + `details.rolled_back`。
  - abort `{stage:"abort", engine_token}` 响应 `{ok, aborted, aborted_token, local:{...}}`。
- **当前值来源**：`mapping` 是 daemon 在 prepare/upload 时记录的方案 routing 镜像（lane->DAC），不是数据面实时回读；方案图被 channels_commit 丢弃或 mirror 未记录时 `mapping=null`，UI 必须显示“当前值未知”并给原因，禁止用旧值冒充当前值。

## 6. 方案文件上传
| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/plan/upload` | 二进制 plan 与 sha 校验头（见 daemon 实现） | 201 首次、200 已存在（dedup）、400 sha 不符；响应 `{ok, stored_path, sha256, dedup}` |

## 7. 校准（calibration）——实际形状（backend 已实现并被测试锁定，本表即 canonical）
现状：uploads/wizard/jobs/artifacts 已可用；生产后端为 JobManager + OfflineJobBackend（`KHAEN_CAL_BACKEND=off` 可关；初始化失败 → not_wired + 真实 reason）。C3b1 起同一 JobManager 也能驱动 **rt-host 真实测量 job**（kind `single_point_rt_host`，见 §7.2；solve/apply 尚未接线，属 C3b2）。**下表是前端必须适配的实际形状**；标【加法】的字段是已落地的增量。

**字段命名（2026-11-26 裁决 H）**：job 对象用 `id`/`status`（crates/daemon tests 已锁定）；只有创建响应与 cancel 回执用 `job_id`，cancel 同时回 `status` 和 `state` 两字段（同源别名）。前端解析可对 job_id|id、state|status 双接受做防御，但契约测试必须断言本表的实际 canonical 形状。

| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/calibration/uploads` | 原始字节 body（不是 multipart；Content-Type 只接受 `application/octet-stream` / `audio/wav`；≤64 MiB）。**上传不解析内容**：plan 字节与 WAV 都走这里，内容格式在使用它的 job 阶段校验 | `{upload_id, sha256, bytes, created}`（201 首次 / 200 已存在去重；`upload_id` = 内容 sha256；**无 schema 字段**）；不支持的 Content-Type 400 M1 |
| GET | `/api/calibration/jobs` | `?limit=&offset=`（1..=200） | `{ok, schema, available, reason?, engine?, active_job_id?, total, limit, offset, jobs:[{id, kind, status, verdict?, created_at, created_unix, started_at?, finished_at?, updated_unix, progress, duration_ms?, error?:{code,message}, reason?}], reasons}` |
| POST | `/api/calibration/jobs` | `{kind:"single_point_offline", inputs:{plan_sha, baseline_upload_id, applied_upload_id, centers_hz:[...], tolerance_db, min_bands}}`（同刻只允许一个 active job，冲突 409 `switch_busy`；`plan_sha` = plan 字节上传到 `/api/calibration/uploads` 得到的 upload_id，不是文件路径） | 202 `{ok, job_id, state, location}` |
| GET | `/api/calibration/jobs/{id}` | — | `{ok, schema, job:{id, kind, status, step?, verdict?, created_at, created_unix, started_at?, finished_at?, updated_unix, progress, duration_ms?, engine?, steps:[{name,status,started_at?,finished_at?,sha256?,detail?}], artifacts:[{name,sha256,bytes,content_type,downloadable}], inputs?, result?, error?:{code,message}, reason?, apply_state, applied_version?, apply}, result, artifacts, error, reasons}`（顶层 result/artifacts/error 与 job 内同源【加法】） |
| POST | `/api/calibration/jobs/{id}/cancel` | — | `{ok, job_id, status, state}`（状态字符串 `cancelled`；status/state 同源别名） |
| POST | `/api/calibration/jobs/{id}/apply` | 旧单发 `{}` / 两阶段 `{stage:"prepare"}`、`{stage:"commit" 或 "abort", engine_token}`（stage 缺省 = 旧单发；未知字段 400；token 只能消费一次） | prepare 200 = 控制面 prepare 体 + 【加法】`{stage:"prepare", job_id, plan_sha256, previous_version, engine_token}`；commit 200 = `{stage:"commit", applied:true, job_id, applied_version, applied_block, graph_id, sha256, previous_version, readback, job, reasons}`；abort 200 = `{stage:"abort", aborted:true, job_id, plan_sha256, control}`；未知/已消费 token 409 `engine_token_invalid` |
| POST | `/api/calibration/jobs/{id}/rollback` | 同 apply | 同 apply |
| GET | `/api/calibration/jobs/{id}/artifacts/{name}` | — | 二进制/JSON 产物（白名单 + manifest sha 重算，校验失败 400/404） |
| GET/PUT | `/api/calibration/wizard` | 配置对象（PUT 为部分字段 patch） | GET `{ok, schema, config, defaults, saved, updated_at, reasons}`；PUT 同形 + 【加法】`set_fields`（daemon 实际写入字段；未知字段 400） |
| POST | `/api/calibration/wizard/apply` | `{job_id?}`（缺省用 last_job_id） | **apply 回执**（与 `/jobs/{id}/apply` 旧单发同形：`{ok, applied:true, job_id, applied_version?, applied_block?, graph_id?, sha256?, snapshot, ack, readback, applied_at, job, reasons}`）；无可用 job 409 `no_applicable_job`，未接线 503 not_wired |
| POST | `/api/calibration/mic/calibration` | 原始 UMIK txt（不是 multipart；≤1 MiB；可选 `X-KHAEN-FILENAME`，默认 `umik.txt`） | 200 `{ok, sha256, filename, bytes, serial, parsed:{serno,sens_factor_db?,points}}`（内容寻址存 `<state_dir>/calibration/mics/<sha256>.txt`；详见 §7.1） |
| GET | `/api/calibration/mic` | — | 200 `{ok, schema:"khaen-mic-profile/v1", profile\|null, reasons:[string]}`（无 profile 时 200 + null + reasons；详见 §7.1） |
| PUT | `/api/calibration/mic` | `{device?, serial?, orientation?, cal_sha256?}`（部分更新；未知字段 400） | 同 GET（保存后的 profile；详见 §7.1） |
| GET | `/api/calibration/mic/status` | — | 恒 200 `{ok, schema, profile\|null, reasons, rt_host}`；`rt_host` 为 rt-host `cal_status` 的实时 UMIK 状态，未接线/不可用时 `available:false` + 稳定 code/reason（不造假；详见 §7.1） |

- **wizard/state 第 2 步（`layout_channels`）**：`/api/channels` 已实现，daemon 不再固定返回 `not_available`/`task=M4`。daemon 按 `/api/status` 的 `format.channels` 判定：可读（非 null、>0）→ `passed`（只读步骤；reason 说明实时映射看 `/api/channels`、编辑用「方案与声道」页）；不可读 → `blocked` + 真实 reason（fail-closed）。

- 状态枚举（canonical）：`queued|running|succeeded|failed|cancelled`（双 l，与实现/现有测试一致；旧文档的 `canceled` 作废）。
- job 审计字段：`job_id, kind, state, created_at/started_at/finished_at`（RFC3339）、`duration_ms, steps, apply, error`；【加法】`created_unix, updated_unix, progress`。
- `measured`/`simulated` 必须如实标记；**模拟结果不得写成实测**。
- 离线 job（`single_point_offline`）不开设备、不出声：JobRequest 对它的 `audible_sweep_authorized` 要求为 false（诚实标注）；真实 UMIK 采集 job 是 §7.2 的 `single_point_rt_host`（daemon 侧已接线；真机窗口实测 NOT_RUN）。
- 离线 job 输入校验（2026-11-27 真 daemon 实测）：plan 的 `sample_rate` 必须等于两段 WAV 的采样率，否则 job failed + 稳定码 `E_OFFLINE_JOB_ALIGNMENT`；WAV 接受 PCM16/24/32 或 float32、1–8 通道（多通道按均值折单声道）、任意非零采样率。
- 预测电平范围：只覆盖 **identity content/matrix/routing + protection PEQ**；plan 带 folds/非恒等段时 job failed + `E_OFFLINE_JOB_LEVELS`（detail 内是 `E_PLAN_LEVELS_*` 稳定码），不静默少算——所以校准 job 的 plan 必须用 identity 方案，不能直接拿 22 通道 audio-profile 生成的折叠方案。
- succeeded job artifacts 至少含 `plan.bin / predicted.json / observed.json / report.json / artifacts.manifest.json`；下载响应头 `x-khaen-artifact-sha256` == 列表 sha == body sha（report schema `khaen-closed-loop-report/v1`，manifest schema `khaen-calibration-artifacts/v1`）。
- apply/rollback 必须按 `host_link.target` 路由（process=进程内、rt-host=控制通道），不得固定打到 daemon 进程内引擎。
- 错误体：成功 2xx + 上述字段；失败 M1 错误体（见 §1）。

### 7.1 麦克风 profile 与 UMIK 校准文件（真实校准桥接 C1/C3a，2026-11-28 更新）

真实校准把“UMIK 序列号 + 校准文件 + 朝向”作为测量输入；C1 做 daemon 侧的 **MicProfile 持久化 + UMIK txt 校验/存储**，C3a 把 `GET /api/calibration/mic/status` 的 `rt_host` 段接到 rt-host 的只读 `cal_status`（经既有 host link，仅 `rt-host` 目标）。status 只读，不触发任何测量/出声；拿不到实时状态时如实降级为 `available:false` + 原因，绝不编造 declared/open。

| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/calibration/mic/calibration` | 原始 UMIK txt（不是 multipart；≤1 MiB；可选请求头 `X-KHAEN-FILENAME`，默认 `umik.txt`，只接受 bare filename，路径分隔符 400）。**不解析 Content-Type** | 200 `{ok, sha256, filename, bytes, serial, parsed:{serno, sens_factor_db\|null, points}}`；内容寻址存 `<state_dir>/calibration/mics/<sha256>.txt`（0600 原子写），同内容重复上传幂等 |
| GET | `/api/calibration/mic` | — | 200 `{ok, schema:"khaen-mic-profile/v1", profile\|null, reasons:[string]}`；无 profile 时 profile=null 且 reasons 至少一条（`no microphone profile saved yet: PUT /api/calibration/mic`） |
| PUT | `/api/calibration/mic` | `{device?, serial?, orientation?, cal_sha256?}`（部分更新；未知字段 400 `calibration_mic_unknown_field`） | 同 GET（保存后的 profile，reasons 为空） |
| GET | `/api/calibration/mic/status` | — | **恒 200** `{ok, schema, profile\|null, reasons, rt_host}`；可用 = `{available:true, reason:null, mode, declared, open, device, declaration_source, reasons, calibration:{simulated, engine}}`，不可用 = `{available:false, code, reason}`（`code` ∈ process_target / host_link_unavailable / unsupported / not_wired / unknown_op / malformed_cal_status_reply / internal_error；`reason` 含 code+message，不吞错） |

- **MicProfile schema**（`<state_dir>/calibration/mic.json`，0600 原子写）：`{schema:"khaen-mic-profile/v1", device, serial, orientation, cal_sha256, cal_filename, cal_path, parsed:{serno, sens_factor_db|null, points}, updated_at}`；`cal_path` 是内容寻址的绝对路径；`cal_filename` 来自上传请求头（只做展示，不参与路径拼接）。
- **REST `reasons` 是字符串数组**（本切片固定为数组；无 profile/损坏时至少一条原因，不冒充已保存）。
- **UMIK txt 解析规则（保守，拒绝而不是猜）**：UTF-8/ASCII 无 BOM、≤1 MiB；**首行必须声明 SERNO**（真实 miniDSP 首行形如 `"Sens Factor =-0.0109999dB, SERNO: 7187696"`，也接受 `SERNO: 123` / `SERNO 123`）；首行 `Sens Factor=<f64>dB` 写入 `parsed.sens_factor_db`，缺失为 `null`；数据前允许空行与注释行（`"`/`#`/`;`/`*`/`'` 开头——真实文件第 2 行就是引号注释）；数据行必须恰好两列可解析 f64，频率有限且 >0、灵敏度有限；至少 1 点、至多 100000 点；不静默截断、不跳过坏行。
- **PUT 语义**：首次保存必须给全 `device`/`orientation`/`cal_sha256`（`serial` 可省略，按文件 SERNO 填；给了就必须与 SERNO 完全一致）；`cal_sha256` 必须指向已上传文件（按内容寻址路径读取并复核字节 sha256）；`cal_filename`/`cal_path`/`parsed`/`updated_at` 由服务端写。`device` 是 ALSA 设备名（如 `hw:CARD=U18dB,DEV=0`）。
- **稳定错误码（M1 错误体，见 §1）**：400 `calibration_mic_invalid`（文件/字段非法，details.field 指向坏行或字段）、400 `calibration_mic_unknown_field`（PUT 未知字段）、400 `calibration_mic_not_found`（PUT 引用的 cal_sha256 没有已上传文件）、400 `calibration_mic_serial_mismatch`（PUT serial 与文件 SERNO 不一致，details 给 serial/serno/cal_sha256）、400 `calibration_mic_orientation_invalid`（不是 `0deg`/`90deg`，details.allowed）、413 `calibration_mic_too_large`（>1 MiB：details.limit_bytes；>100000 点：details.limit_points）。
- **存储与安全**：磁盘文件名只由字节 sha256 决定，客户端文件名不参与路径拼接；`mic.json` 与校准文件/回执均 0600 原子写（同目录 `.tmp-*` → rename）；文件不存在/hash 不符 400，不拿默认值冒充。
- **status 实时段（C3a，2026-11-28）**：`rt_host` 的字段逐个映射 rt-host `cal_status` 回执，daemon 不重算、不补值：`mode` ← 顶层 `mode`（`playback`/`calibration`）；`declared`/`open`/`device`/`declaration_source` ← `umik.declared`/`umik.open`/`umik.device`/`umik.declaration_source`；`calibration.{simulated,engine}` ← 顶层 `simulated`/`engine`；`reasons` ← 顶层 `reasons.mic`（UMIK 声明缺失原因，字符串或 null）。字段缺失/显式 null 时如实 null；回执缺 `umik` 或字段类型不符按畸形回执降级，不当成功。`declaration_source` 的来源是 rt-host：`cal_enter_override`（该会话 `cal_enter` 请求内的 mic 覆盖）/ `env`（rt-host 环境变量声明）/ `missing`（没有 UMIK 声明）。
- **status 降级语义（C3a）**：只有 `rt-host` 目标有实时状态——daemon 经既有 host link 发 `{op:"cal_status"}`（合法 op 表 `CAL_OPS` 与 `crates/rt-host/src/control/cal/mod.rs` 同源；`cal_status` 只读可重试，其余 `cal_*` 非幂等不重试）。`process` 目标没有校准能力 → `available:false` + `code:"process_target"`；rt-host 未接线（`KHAEN_CAL=off`）→ `code:"unsupported"`；socket 不可用 → `code:"host_link_unavailable"`；畸形回执 → `code:"malformed_cal_status_reply"`。这些降级**不改变 HTTP 200**，也**不影响** `profile`/`reasons`（仍来自 `<state_dir>/calibration/mic.json`）。

### 7.2 rt-host 真实测量 job（`single_point_rt_host`，C3b1，2026-11-28）

真实测量由 rt-host 在真实设备上播放扫频、由 UMIK 采集；daemon 只经既有 host link（C2/C3a 同一个控制通道）驱动 `cal_enter -> cal_measure -> cal_status -> cal_leave`，并把 **rt-host 真实回执**落成证据产物。本切片**只做测量**：solve/apply（方案设计、两阶段应用）与闭环质量判定属 C3b2，**job 不报 PASS、没有可应用 plan**。

后端选择（`KHAEN_CAL_BACKEND`；`off`/`0` 仍由 main 关停）：未设置/`offline` = 现状离线 job；`rt-host` = 真实测量模式（离线 job 仍可用）；`auto` = 启动时做一次只读 `cal_status` 探测，成功进 rt-host，失败**降级 offline** 并把真实原因写进 `/api/status` 的 calibration reason 与创建错误（不静默）。真实测量还要求 `KHAEN_RT_TARGET=rt-host`（process 目标没有校准数据面）。

| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/calibration/jobs` | `{kind:"single_point_rt_host", inputs:{audible:true, channels?:[u32], level_dbfs?:f64}}` | 202 `{ok, job_id, state, location}`（与离线 job 同形） |

- **inputs 校验（400 `invalid_request`，`details.field` 指向字段）**：`audible` 必须存在且为 `true`（P08：真实测量会出声/采音，静默请求一律拒绝）；`channels` 可选，非空、≤22 项、非负整数、不重复（缺省=rt-host 自己的 `KHAEN_CAL_CHANNELS`）；`level_dbfs` 可选，有限且在 `[-60, -1]`（缺省=rt-host 自己的电平）；未知字段 400（不静默忽略）。
- **创建期校验（不排队假 job）**：后端必须是 rt-host 模式且控制通道可用，否则 503 `not_wired` + 稳定 reason；`MicProfile` 必须已保存（否则 **409 `mic_profile_missing`**，message 带缺失/损坏原因）；同一时刻已有 active job → 409 `switch_busy`。
- **wizard/state 联动**：最近 job 为 `single_point_rt_host` 时，第 5 步不再要求两段离线 WAV——job allocated/running 时 `blocked`（可在第 5 步启动并轮询），`succeeded` 时 `passed`；第 6–8 步（analysis/design/apply）在 C3b1 阶段固定 `not_available` + task `C3b2`，**不因测量成功而放行**。
- **job steps（4 步，顺序固定）**：`validate` → `sweep_capture` → `quality_check` → `bundle`。详情 `steps[].name` 就是这四个机器名（与离线 job 的 5 步不同）。
  - `validate`：目标必须是 rt-host；MicProfile 必须可用且校准文件可读、sha256 与声明一致；`audible_sweep_authorized` 必须 true；再做一次只读 `cal_status` 预检（设备缺失/未启动/静音/合成引擎(simulated)/rt-host 未接线都在进入校准模式前失败）。失败稳定码：`E_MIC_PROFILE` / `E_RT_HOST` / `E_AUDIBLE` / `UNAVAILABLE`。
  - `sweep_capture`：`cal_enter{audible:true, mic:{device,serial,cal_path,cal_sha256,orientation}, channels?, level_dbfs?}`（`mic` 来自已保存的 MicProfile，C2 的 per-session 覆盖）→ `cal_measure{audible:true}`（异步 accepted，取 `measure_id`/`session_id`）→ 每 **250 ms** 轮询一次 `cal_status`（总超时默认 **30 min**，可配置），每轮检查取消令牌；`done` 时写 `measurement.json`。**任何退出路径（成功/失败/取消）都会调用 `cal_leave`**（C3a 已定：`cal_enter/cal_measure/cal_leave` 不重试）。
  - `quality_check`：读 `measurement.json`，只搬运 rt-host 真实给出的 `quality`/`verdict` 字段。rt-host 当前 `cal_status` 只提供测量摘要（`snr_db`/`capture_peak_dbfs`/`xruns`/`clipped_samples`/`valid_slots`/`total_slots`），没有闭环质量门对象 → `quality.json` 写 `available:false` + `verdict:null` + 原因，**绝不写假 PASS**。
  - `bundle`：`cal_leave` 幂等（成功路径只调用一次；restart 后 resume 到 bundle 时补一次）→ 写 `report.json`（真实 job id/`measure_id`/artifact sha256；`verdict` 恒 null 并在 notes 说明 solve/apply 属 C3b2）。
- **artifacts（白名单 + manifest sha 重算校验，均可下载）**：`measurement.json`（schema `khaen-calibration-rt-host-measurement/v1`；逐字段拷贝 rt-host `cal_status` 的 `measure` 对象与 `engine/simulated/real_capture/umik` 快照，缺失即 null，不补默认值）；`quality.json`（schema `khaen-calibration-rt-host-quality/v1`）；`report.json`（schema `khaen-calibration-rt-host-report/v1`）；`artifacts.manifest.json`（JobManager 终态写，schema `khaen-calibration-artifacts/v1`）。
- **详情加法字段**：`inputs_rt_host:{audible, channels, level_dbfs}` 回显会话输入；`inputs` 为 null（rt-host job 不读上传录音），`reasons` 说明；`result`/`verdict` 为 null + `reasons` 说明本切片不做闭环判定（`engine` 来自 rt-host 回执）。
- **取消**：`POST /api/calibration/jobs/{id}/cancel` 协作式推进；轮询循环每 250 ms 检查取消，随后 `cal_leave`，job 终态 `cancelled`（不产生 `measurement.json`）。
- **失败语义**：rt-host 业务错误码原样进 job `error.code`（如 `measure_failed`、`E_QUALITY_SNR`、`mic_declaration_invalid`、`mic_cal_file_invalid`、`device_absent`、`audio_blocked`、`format_unsupported`），`error.message` 同时保留 code/stage_code/人读原因；daemon 侧稳定码 `E_RT_HOST`（控制通道不可用/目标不对）、`E_CAL_STATUS`（`cal_status` 回执畸形）、`E_CAL_MEASURE`（`cal_measure` 回执缺 measure_id/session_id）、`E_MEASURE_TIMEOUT`、`E_CAL_LEAVE`（成功测量后 leave 失败）、`E_MIC_PROFILE`、`UNAVAILABLE`。`cal_leave` 失败不掩盖原错误：原错误保留，message 追加 leave 失败，并尽力写 `cal_leave.json` 证据。
- **apply/rollback**：rt-host 测量 job 没有可应用 plan → `POST .../apply` 返回 409 `plan_unavailable`（message 说明 solve/apply 属 C3b2）；**不得显示 PASS**。
- **daemon 重启**：rt-host 测量会话状态在 rt-host 内存里，不可跨进程 resume；重启时尽力 `cal_leave` 后把 pending job **abandon 成 failed** 并带真实原因（不假装还在跑）。
- **设备/出声**：只有目标=rt-host、设备存在且未静音才会进入校准模式；可闻授权由请求显式给出。daemon 侧全链路用脚本化 rt-host 回执测过；**真机 UMIK/MOTU 窗口实测是 NOT_RUN**（需出声窗口与用户授权）。

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
- **G3 当前值**：暂不新增 GET /api/surface 读接口；除 routing 可用 `/api/channels.mapping`（标注来源/降级）外，其余显示“当前值不可读”。**例外（v1.2 加法）**：音频模式当前值现可从 `GET /api/status.mode` 读到（来源=rt-host 控制 status 顶层 mode，仅 socket；file/process/不可用时 null+reason）；UI 有真值显示真值，null 时如实显示“不可读+原因”。可读 surface 状态其余项仍为后续独立任务。
- **G5**：以本文件 §7 的“实际形状 + 加法目标”为准（前端先适配实际形状）。
- **G6**：routing 当前值来源与降级口径见 §5 末条与 §9.7。
- **H2（v1.2，认证 §2 形状，2026-11-27 实测）**：以上表 §2 实际形状为准——`/api/auth/status` 是 `{configured,authenticated}`（无 ok/initialized）、setup 201、login/session/password 的 token 响应是 `{csrf, expires_in}`、session 是 `{authenticated,csrf,expires_at}`、logout 200 `{ok:true}`。前端（webui/src/api/auth.ts）已按此实现；本次由**真 daemon 本地冒烟**核实（见 HANDOFF/20261126-khaen-engineering-integration/evidence/local-daemon-smoke.json）。
- **H3（v1.2 补充，2026-11-27 真 daemon 实测）**：校准 §7 离线 job 全链路核实（profile→plan→plan 上传→WAV 上传→建 job→succeeded→5 artifacts 下载→两阶段 apply prepare→commit 生效）；两阶段 apply 实际回执形状见 §7 表，plan 必须作为 calibration upload 上传（plan_sha=upload_id）；`/api/audio-profile/plan` 生成的 22ch 方案在当前 2ch 引擎下 inspect.compile 如实报 `content channel mismatch`（不假成功）。证据：HANDOFF/20261126-khaen-engineering-integration/evidence/daemon-calibration-e2e.json。不升 api_version。
- **H（v1.2，校准 §7 字段）**：以**后端实际形状**为准修正 §7——job 对象 canonical=`id`/`status`（backend tests 已锁定，不改后端加别名）；创建响应=`{ok, job_id, state, location}`、cancel=`{ok, job_id, status, state}`（status/state 同源别名）；job 详情顶层 result/artifacts/error；uploads 返回 `{upload_id,sha256,bytes,created}`（无 schema）；wizard/apply 返回 apply 回执（无 location）。前端解析保持宽容（job_id|id、state|status）但契约测试断言实际 canonical 形状；本修正不改任何实际行为，不升 api_version。

## 12. 音频通道 profile（输入/输出映射与布局）——v1.2 新增
状态（v1.2，2026-11-27）：**daemon 后端已实现并有测试锁定**（`crates/daemon/src/audio_profile/`、`speaker_test.rs`）——GET `/api/audio-profile` 200（默认 2.0 布局 + `schema`/`validation`）；PUT 两阶段 prepare→commit/abort（一次性 token，120 s TTL）；POST `/api/audio-profile/plan` 生成 22 通道 ABI v3 方案（滤波器来源 identity，inspect 的 compile 结果如实返回，不假成功）；`GET /api/device/inputs` 返回 `available:false` + 真实 reason（ALSA capture 枚举尚未实现，不编列表）。`POST /api/speaker-test` 由 `KHAEN_SPEAKER_TEST` 控制：off → 503 `not_wired`+reason、窗口外 → 403 `sound_window_closed`、正常时转发 rt-host `test_tone`（默认 -30 dBFS、500..=10000 ms、必须 `audible`+`confirm`）；`POST /api/speaker-test/stop` 不按开关/窗口拦截，但 process 目标如实 503。新增端点一律 M1 错误体、成功带 `ok:true`。

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
| GET | `/api/audio-profile` | — | `{ok, schema, profile, validation:{ok, errors[], warnings[]}, capability:{aes67:{channels, map, channel_order?, channel_order_source:"sdp"|"manual"|"none"}, device_inputs:{available, optical_mode:"spdif"|"adat"|null, sample_rate, channels:[{index, label?}], reason?}}, plan:{lanes, enabled_outputs, dac_channels, graph_id?}, revision, saved, reasons}`；read-only 永远 200，不可用字段 null + reasons |
| PUT | `/api/audio-profile` | `{stage:"prepare", profile}` → `{ok, stage:"prepare", profile, validation, diff:{outputs_changed, inputs_changed, downmix_changed, fanout_changed}, token, expires_in_s}`（不碰引擎）；`{stage:"commit", token}` → `{ok, applied:true, saved:true, profile, revision}`；`{stage:"abort", token}` → `{ok, aborted:true}` |
| POST | `/api/audio-profile/plan` | `{}` | `{ok, plan_path, sha256, lane_count, inspect, folds_preview:[{source, channel, role, lane}], downmix_applied:[{kind:"center"|"lfe", gain_db, lanes:[...]}]}`；之后走现有 `/api/prepare` + `/api/commit`（不自动提交） |
| GET | `/api/device/inputs` | — | `{ok, available, optical_mode, sample_rate, channels:[{index,label?,kind:"analog"|"optical"}], source:"motu-alsa", alsa_capture:[{name,description,card,device,channels_min,channels_max,open_error}], reason?}`（模拟 + 光口全部算声卡输入通道）；**`alsa_capture` 为【加法】字段**：真实枚举到的 ALSA capture PCM（设备名/通道数范围/打开错误），Linux 上由 alsa crate 只读探测，非 Linux 为 `null`；ALSA 不暴露逐通道 analog/optical 角色，所以 `channels` 在拿到设备级元数据源前保持 `null` + reason，不编数值 |
| GET | `/api/channels` | — | 加法字段 `speaker_roles:[{dac, role, enabled}]`、`input_roles:[{source, channel, role}]` |
| POST | `/api/speaker-test` | `{dac:0..21, ms:500..10000, kind:"noise"|"sine", db?:number, freq_hz?:number, audible:true, confirm:true}` | `{ok, dac, ms, kind, freq_hz, level_db, total_blocks, remaining_ms}`（rt-host 实际回执，透传）；默认 db=-30，范围 -60..-12 |
| POST | `/api/speaker-test/stop` | `{}` | `{ok, stopped:boolean}`（幂等）；无测试音时另带 `reason`，有测试音时另带 `was_active` |
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