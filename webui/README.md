# khaen webui

khaen DSP 引擎的 Web 控制台：Vue 3 + TypeScript + Element Plus + Vite + vue-router + vue-i18n。
由 khaen-daemon 同源托管（编译期 `include_bytes!` 内嵌 `webui/dist`），运行期不需要 Node / Python / 反代。

> 规划：`HANDOFF/20261126-khaen-webui-plan/PLAN.md`（仓库外）§4 API、§5 路由、§6.1 登录、§6.2 概览、§11 UI 布局、§12 主题与 i18n。
> 当前里程碑：**T62（前端工程补齐，2026-11-26）**——五个语义界面真实两阶段可编辑、MOTU 控制面、校准 UI 按 §7 实际形状接线、
> 声道热重配两阶段、新增「工作模式 / 内容布局 / 音频映射（Speaker Layout）」页、i18n 分片机制与过期文案清理、契约测试。
> API 合同唯一来源：`docs/API.md`（v1.2，lead 维护，含 §11 裁决记录与 §12 音频 channel profile）；设计：`docs/SPEAKER-MAPPING.md`。
> 真实 Rust 后端联调、真机出声/热切换、MOTU 实机写入由 lead 在合并后执行（本任务 NOT_RUN 清单见文末；前端用契约 mock + 单测 + vite build 验证）。

## T62：前端工程补齐（当前实现基线，2026-11-26）

> M1–M6 各节保留为历史记录（记录当时的实现细节）；T62 之后的页面/契约以本节与 `docs/API.md` 为准。

### 页面（13 个路由）

| 路由 | 页面 | 数据面 |
|---|---|---|
| `/login` | 登录/初始化 | `/api/auth/*` |
| `/` | 概览 | `GET /api/status`（5s 轮询） |
| `/volume` | 音量/静音 + MOTU | `POST /api/mute`；`GET /api/motu`+`PUT /api/motu/volume`（prepare→commit，无 abort：本地取消+TTL）；`/api/surface volume` 两阶段 |
| `/mode` | 工作模式（新） | `/api/surface mode` 两阶段（off/aes67；on=aes67 别名；analog 不提供） |
| `/channels` | 方案与声道 | `GET/PUT /api/channels`（khaen-channels/v1 全形状 + 两阶段热重配；mapping 是方案镜像，mapping=null 显示“当前值未知”）、`/api/plan/*`、`/api/prepare|commit|abort|rollback` |
| `/routing` | 路由矩阵（升级为可编辑） | `GET /api/channels.mapping` 当前值（标注来源/降级）；`/api/surface routing` 两阶段 + `/api/commit|abort` |
| `/layout` | 内容布局（新） | `/api/surface layout` 两阶段（in_channels/out_lanes/folds）；内容布局是方案补丁，当前值不可读时如实标注 |
| `/speakers` | 音频映射 / Speaker Layout（新） | `GET/PUT /api/audio-profile` 两阶段、`POST /api/audio-profile/plan`（→inspect→prepare→commit）、`GET /api/device/inputs`、`POST /api/speaker-test(+stop)`（audible+confirm，默认 -30 dBFS/1 s，可立即停止） |
| `/format` | 音频格式 | `/api/format` 两阶段（changed/format/device_restart_required）、`/api/device/retry` |
| `/calibration` | 校准列表 | §7 实际形状：jobs `{schema,available,reason,engine,...}`；`/api/calibration/jobs/{id}/apply|rollback`（单发 `{}` + 兼容 staged）；artifacts 带认证无损下载（`getRawBytes` → Blob） |
| `/calibration/wizard` | 8 步向导 | `/api/calibration/wizard`、`.../wizard/state`、`.../wizard/apply` |
| `/calibration/jobs/:id` | job 详情 | `GET /api/calibration/jobs/{id}`（canonical `id/status`）、cancel/apply/rollback |
| `/system`、`/system/logs` | 系统信息/日志 | `/api/system/*` |

### 共享基础设施

- **i18n 分片**：基础 `src/i18n/zh-CN.ts`/`en-US.ts` + `src/i18n/messages/<area>.<locale>.ts`（draftReview/meta/motu/mode/layout/routingEdit/calibrationEdit/channelsEdit/speakerRole/speakerLayout/speakerTest），
  `i18n/index.ts` 合并；`scripts/i18n-check.mjs` 校验跨文件键集对等、无重复键、源码引用存在、注释行/E2E 文案不误判；
- **能力快照**：`GET /api/meta`（`src/api/meta.ts` + `src/stores/meta.ts`，AppShell 启动预热）；`surfaces/not_wired` 三态（wired/not_wired/unknown）驱动入口，unknown 降级但标注；
- **两阶段标准件**：`src/components/control/DraftReview.vue`（prepare 回执：changed/diff/current/target/graph/admission/blockers；缺省显示“服务端未上报”）；
- **错误语义**：`ApiClient` 同时兼容 M1 `{error:{code,message,details}}` 与 host_link 转发 `{ok:false,error:"<code>",message}`；401 统一跳登录；501/503 显示“未实现/未接入 + reason”不假成功；
- **二进制下载**：`ApiClient.getRawBytes`（无损 ArrayBuffer，用于校准产物 plan.bin 等）；
- **诚实边界**：当前值不可读（volume 逐 lane 增益、layout 内容、mode 当前值）一律显示“不可读（无数据源）”；routing 用 `/api/channels.mapping` 并标注来源，mapping=null 显示未知；未使用 DAC 不做 CueMix 写入。

### 契约测试

- `tests/api-contract.test.ts`：解析 `docs/API.md` 的端点表/列表，断言 webui 实际调用的每个 path/method 都在文档中；
- `tests/navigation.test.ts`：侧栏项 ↔ 路由注册 ↔ 双语言标题 ↔ requiresAuth 一致性；
- 各区域 mock 契约测试：断言 POST/PUT 的 path/method/body 精确形状、prepare/commit/abort、401/501/503、失败回执（token 单次消费、commit_supported=false 不提交、MOTU 无 abort 等）。

## 当前状态（历史：M1–M6）

### M1：认证 / 会话 / 路由守卫（已合并）

- 工程骨架：Vite 6 + Vue 3.5 + TS 5.9 + Element Plus 2.14 + vue-router 4 + vue-i18n 11（pin 版本，见 `package.json`）；
- 深/浅双主题：PLAN §12.1 的 token 全量落地（`src/styles/tokens.css`），首帧由 `public/theme-init.js` 防闪，
  三态（跟随系统/深色/浅色）持久化到 `localStorage['khaen.theme']`，顶栏一键切换；
- i18n：`zh-CN`（源语言，完整）+ `en-US`（键集对等），全部界面文案走 `t()`，
  `scripts/i18n-check.mjs` 校验键集对等 / 重复键 / 空值 / 源码引用 / 硬编码中文；
- 认证 API 客户端（`src/api/client.ts` + `src/api/auth.ts`）：credentials include、CSRF 自动头、统一错误体解析、
  401 统一回调、API 版本头校验、网络/超时归类；
- 会话 store（`src/stores/session.ts`）+ 路由守卫（`src/router/guard.ts`）：两段探测、并发去重、过期重查、
  防开放重定向、fail closed；
- 登录 / 初始化页（`src/views/LoginView.vue`）两态 + 401/429/409/不可达文案 + 回车提交 + aria-live；
- AppShell 登出。

### M2：概览页接 `GET /api/status`（本任务 T35b）

数据链路（**页面唯一数据源**，不读假数据）：

```
src/api/status.ts        khaen-status/v1 类型 + 运行时校验（schema/类型漂移 -> invalid_response）+ 端点封装
                         + 派生纯函数（isStatusStale / runStateKind / underrunRate / uptimeParts / reasons 查询）
src/stores/status.ts     共享状态（data/phase/error/refreshing/loadedAt）+ 5s 轮询控制器（可注入时钟/定时器/fetch）
src/composables/useStatusPolling.ts
                         Vue 生命周期 + visibilitychange 接线：隐藏暂停并中止在途请求，恢复可见立即刷新
src/utils/format.ts      Intl 数字/时间格式化、id 中间省略、字节/采样率换算（locale 显式传入，可单测）
```

页面行为：

- 概览页 8 张卡（PLAN §11.4 顺序）：**运行状态 / 输入统计 / 输出统计 / 当前方案 / 格式与设备 / AES67 / 最近校准 / 控制通道**，
  全部字段来自 `/api/status`；null 字段显示「—」+「未接入」TruthTag（tooltip 显示 `reasons` 里的原因），不编造数值；
- 顶部状态带：运行状态徽标（running/faulted/muted/safe）+ 最近错误（`state.last_error`）+ 控制通道徽标 + 最后更新时间；
- 5s 轮询：页面可见时 5s；`visibilitychange` 隐藏 -> 暂停（停止定时器 + abort 在途请求）；恢复可见 -> 立即刷新再继续；
- 过期/断线：`host_link.connected === false` 或 `status_age_ms > 2000` -> 黄色徽标（已断开（状态过期）/ 已过期）+ 警示条（显示最后成功数据）；
- 错误：无数据 -> 错误卡 + 重试；有数据但刷新失败 -> 警示条（保留最后成功数据 + 最后更新时间），不清空/造假；
- 401：交给共享 ApiClient -> 会话 store 清态并跳 `/login`，概览页不自行处理；
- 加载骨架：首屏 8 张骨架卡；手动刷新按钮（页面头部 + 顶栏）共用同一在途请求（并发去重）；
- 顶栏徽标接真实值（运行状态 / 方案版本 / 设备速率），侧栏底部控制通道小圆点（已连接绿 / 过期黄 / 未接入灰）；
- `not_wired` 列表在卡片下方如实展示（后端上报的未接入能力），不展示假数据；
- 深/浅主题、390x844 / 1440x900 无横向溢出（浏览器实测，见"视觉验收"）。

### M3：音量/静音、音频格式、工作模式（本任务 T53）

页面与路由：

| 路由 | 名称 | 页面 |
|---|---|---|
| `/volume` | volume | 静音开关（POST /api/mute，立即生效 + /api/status 回读）+ 逐 lane 保护增益草稿（22 lane，来自 `format.channels`）+ 工作模式卡（off/aes67 两阶段；analog NOT_AVAILABLE）+ 主音量 NOT_AVAILABLE 说明 |
| `/format` | format | 当前格式（plan_rate/device_rate/rate_asserted/block/channels/encoding/AES67）+ rate_asserted=false 时「重试回读」+ 两阶段格式变更（48k/96k、64/128）+ 重启 rt-host 才在真实设备生效的明确说明 |

数据链路（页面唯一数据源，不读假数据）：

```
src/api/control.ts        控制面 REST 类型 + 运行时校验（prepare/commit/abort/mute/device retry 回执取并集）
                          + 逐 lane 增益模型（clamp/0.5dB 步进/勾选补丁校验）+ 错误码归类（host_link_unavailable/
                          unsupported/engine_token_invalid/no_base_plan/admission_rejected/...）
src/stores/control.ts     单草稿槽两阶段状态机（prepare→draft→commit/abort）+ 静音/设备重试 + 最近 8 条操作历史
src/views/VolumeView.vue  静音、逐 lane 草稿、模式卡、两阶段回执/历史
src/views/FormatView.vue  当前格式、重试回读、两阶段格式变更、重启提示
src/components/control/ControlErrorNotice.vue  错误条（稳定码 + 本地化主文案 + 详情 + 重试）
```

契约（`crates/daemon/src/lib.rs` + `crates/host/src/control/mod.rs` 的**真实实现**；T70f-b 转发 rt-host）：

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/mute` | `{mute}` → `{ok,mute,applied_block,idempotent}`；立即生效，页面刷新 `/api/status` 回读 |
| POST | `/api/surface` | `{surface:"volume", data:{gains_db:[{lane,gain_db}]}}` → `{ok,surface,engine_token,planned_version,graph_id,admission}`（只准备） |
| POST | `/api/surface` | `{surface:"mode", data:{mode:"off"\|"on"\|"aes67"}}` → `{engine_token,planned_version,mode,changed,...}`（只准备） |
| POST | `/api/format` | `{sample_rate?,chunksize?}` → `{engine_token,planned_version,format,changed,device_restart_required:true,note}`（只准备） |
| POST | `/api/commit` | `{engine_token}` → 生效回执（volume: version/applied_block/graph_id；mode: changed/mode；format: changed/format/graph_dropped/history_cleared/mute_preserved） |
| POST | `/api/abort` | `{engine_token}` → `{ok,aborted,aborted_surface?}`（丢弃草稿） |
| POST | `/api/device/retry` | 设备监督器立即重试一次（rate_asserted=false 时页面「重试回读」） |

诚实边界（页面逐条明示）：

- **主音量 NOT_AVAILABLE**：控制协议（T70f-a/b）没有 master volume op，`/api/surface volume` 只接受 per-lane 增益；
  页面不把「写所有 lane」伪装成主音量（会覆盖逐 lane 修剪），改为逐 lane 草稿并标注任务号；
- **当前 per-lane 增益不可读**：没有读取保护段增益的 op，草稿从 0 dB 中性值开始，当前值标「未接入」，不编 0 dB；
- **模式当前值不可读**：`/api/status` 不含 mode，页面只显示最近一次 commit 回执里的 mode（并注明来源）；
- **format commit 不是设备生效**：响应显式 `device_restart_required`，页面把「控制面回执」与「设备回读 device_rate」分栏显示，
  重启 rt-host 前不声称设备已按新速率运行；`encoding` 无数据源时显示「未接入」；
- **safe/faulted 只读规则**：音量/模式/格式写入被禁用（避免故障下继续写），但**静音开关保留**——设备安全锁存的恢复路径
  就是显式 `/api/mute false`（`crates/daemon/src/device.rs`），不能把恢复入口一起关掉；
- 未接通/不可用返回 501/503 + 稳定码：页面显示 NOT_AVAILABLE + 原因 + 任务号，不假成功。

### M4：方案与声道（本任务 T54）

页面与路由：

| 路由 | 名称 | 页面 |
|---|---|---|
| `/channels` | channels | 当前方案摘要（/api/status + /api/plan/history）、输出通道与 MOTU 物理映射（22ch，已知位置 L/R=1-2、SL/SR=3-4、TF/TFR=5-6、LFE=8）、AES67 输入（channels/map/source/sdp_sha256）、方案上传（sha256 双侧校验 → inspect → prepare → commit）、历史与回滚、声道热重配（M3b 未完成时 commit 501 NOT_AVAILABLE） |

数据链路：

```
src/api/channels.ts        khaen-channels/v1 校验 + 方案上传/历史/预览解析（T60：daemon 真实形态 segments 数组/
                          current 对象/recorded_at/可空 dac_map + 旧 T54 mock 兼容；inspect 是 POST plan_path）+ MOTU 标签纯函数
                          + 客户端 WebCrypto sha256 + 声道补丁校验 + 错误归类
src/stores/channels.ts     声道读取控制器 + 方案上传/预览/两阶段（prepare→commit/abort）+ 回滚草稿 + 历史；单写操作互斥
                          （inspect/prepare 用上传回执的 stored_path；没有时如实 NOT_AVAILABLE plan_path_unavailable）
src/views/ChannelsView.vue 方案摘要 / 输出映射表 / AES67 输入 / 上传卡 / 历史回滚表 / 热重配卡 / 两阶段草稿回执
```

契约（**T60 已按 `crates/daemon/src` 真实实现对齐**；旧 T54 mock 契约仍在解析层保留宽松兼容）：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/channels` | `khaen-channels/v1`：`{output:{channels,dac_map}, input:{channels,map,source,sdp_sha256}, mapping, plan:{source,version,graph_id,routing_known}, reasons, not_wired}`；`dac_map` 允许 null 项（该 DAC 无 lane 喂入，**合法**），`mapping:null` = 当前方案 routing 段未知（≠「已知 0 条路由」）；缺失字段 null + reason |
| PUT | `/api/channels` | **T60 未对齐（未做项）**：真实 daemon 是 `{stage:"prepare"\|"commit", mapping:[{lane,dac}]}`（commit 在 M3b 前 501 `host_link_unsupported`），与 T54 的 `{output:{channels,dac_map},input:{...}}` 补丁形态不同；页面把该写入口明示为 NOT_AVAILABLE（T52/T55），不假成功 |
| POST | `/api/plan/upload` | 原始 plan 字节（≤ 4 MiB）→ daemon 真实回执 `{ok,schema,sha256,graph_id,segments:[区段名],segment_count,latency_samples,stored_path,...}`（首次 201 / 同 sha 200 幂等；**无 upload_id/version**） |
| POST | `/api/prepare` | `{plan_path,plan_sha256}` → 方案草稿（engine_token/planned_version/graph_id；plan_path 来自上传回执的 `stored_path`） |
| GET | `/api/plan/history` | daemon 真实形态 `{schema,source,current:{version,graph_id},entries:[{version,graph_id,applied_block,recorded_at,source,sha256,latency_samples,observed}],pagination,reasons}`；前端兼容旧 `{current_version,versions}`/数组形态，页面把 `recorded_at` 归一化为应用时间 |
| POST | `/api/plan/inspect` | **POST** `{plan_path,plan_sha256?}`（不是 GET/query；plan_path = 上传回执的 stored_path）→ 只读预览（plan + admission + compile）；no path 时页面如实 NOT_AVAILABLE（`plan_path_unavailable`） |
| POST | `/api/rollback` | `{target?}` → 回滚草稿（rolled_back_to；版本号递增，历史不改写），仍需 commit |
| POST | `/api/commit` / `/api/abort` | 共享两阶段提交/丢弃（channels 草稿在 M3b/T52 完成前 commit 返回 501 `host_link_unsupported`） |

T60 契约对齐要点（前端解析层，单测覆盖真实形态 + 旧 mock 兼容）：

1. `/api/plan/history`：接受 daemon 的 `current:{version,graph_id}` + `entries[]`（`recorded_at`）与旧 `current_version/versions`；
   daemon 显式上报 `current.version=null` 时**不拿条目最大版本冒充**当前版本，原因来自 `reasons['history.current']`；
2. `/api/plan/upload` 回执：`segments` 是区段名数组 + `segment_count`；旧 mock 的「数字 segments」归一化为段数，
   不拿数字伪造区段名；上传回执的 `stored_path` 是后续 inspect/prepare 的唯一 plan_path 来源，缺失时页面 NOT_AVAILABLE；
3. `/api/plan/inspect`：改为 POST `{plan_path,plan_sha256}`；没有 plan_path 时本地就拒绝（不发猜测请求）；
4. `/api/channels` 的 `dac_map` 允许 `(number|null)[]`（null = 该 DAC 静音/无 lane），`mapping:null` 如实显示
   「routing 未知」而不是「空映射」；页面逐字段 NOT_AVAILABLE + 后端 reason。

诚实边界（页面逐条明示）：

- **M4 后端未接入（T55）**：/api/channels 与 plan upload/history/inspect/rollback 返回 501 `not_wired` 时，页面整块
  NOT_AVAILABLE + 稳定码 + 原因 + 任务号（T55），不编造映射/历史；
- **热重配未完成（M3b/T52）**：PUT prepare 可返回 preview/warnings/blockers，但 commit 会被后端以 501
  `host_link_unsupported` 拒绝；页面如实显示错误并写明任务号，绝不显示成功；变更前警告「会中断出声」（预计 1–2 s）；
- **sha256 双侧校验**：上传前浏览器 WebCrypto 计算，上传后与服务端返回比对；不一致即拒绝 prepare（不信任任何一侧）；
- **MOTU 标签只标已知位置**：1-2 L/R、3-4 SL/SR、5-6 TF/TFR、8 LFE；未知位置显示 `OUT n`，不猜；
- **历史/预览不编造**：/api/plan/history 不可用时清空历史并显示原因；inspect 不可用时只显示上传摘要；
- 回滚 = 版本号递增的新提交（历史保留），必须 commit 才生效；提交/回滚均有二次确认。

### M3b：路由矩阵只读页（本任务 T58）

页面与路由：

| 路由 | 名称 | 页面 |
|---|---|---|
| `/routing` | routing | 输入 → 22 lane 矩阵域 → DAC 物理输出的只读诊断：路由链 3 节点、lane → DAC 映射表（与 /channels 同口径 + 跳转）、计划内 routing 段摘要、编辑/重排 NOT_AVAILABLE（T59）、降级/未接线诚实展示 |

数据链路（**不新增后端端点**，只组合现有只读接口）：

```
src/api/routing.ts        宽松解析（真实 daemon 形态：dac_map 允许 null、mapping null = routing 段未知、
                          /api/plan/history 兼容 {current:{version},entries} 与 {current_version,versions}）
                          + 组合纯函数（buildRoutingChain / buildPlanSummary / laneRows / reasons 查询）
src/stores/routing.ts     channels + history 只读控制器（并行拉取、并发去重、失败保留最后成功数据、
                          history 失败清空，不用旧版本冒充当前版本）
src/views/RoutingView.vue 路由链 / lane→DAC 表 / 计划摘要 / 编辑 NOT_AVAILABLE / 未接入条目
```

数据来源（页面唯一事实源）：

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/api/status` | 5s 共享轮询：`format.channels`（矩阵 lane 数）、`format.aes67.{channels,map}`（输入）、`plan.{version,latency_samples,applied_at,source,graph_id}`、`host_link`、`not_wired/reasons` |
| GET | `/api/channels` | `output.channels/dac_map`（DAC 输出）、`mapping[{lane,dac}]`（lane→DAC；null = routing 段未知）、`input.{channels,map,source,sdp_sha256}`、`plan.{source,version,graph_id,routing_known}`、`reasons/not_wired` |
| GET | `/api/plan/history` | `current_version` + entries（版本交叉核对、latency 回退来源、applied_at） |

诚实边界（页面逐条明示）：

- **没有 mapping 不假设直通**：routing 段未知时 daemon 上报 `mapping: null` → 表格空 + 「未接入」+ 原因，
  绝不画 lane i → DAC i；`dac_map` 的 null 项表示该 DAC 无 lane 喂入（合法形态），不补 0；
- **segments（区段数）没有只读数据源**：daemon 只在 `POST /api/plan/upload` 回执里报告 `segments/segment_count`，
  `/api/status` 与 `/api/plan/history` 都不含它；页面如实 NOT_AVAILABLE + 原因，绝不拿路由条目数冒充区段数
  （上传/查看入口在 `/channels`）；
- **路由编辑/重排 NOT_AVAILABLE（T59）**：需要 PlanGraph 编辑器/designer（交叉点编辑、lane 重排、Plan 区段编译）；
  三个写按钮禁用 + 原因；控制面低层 `POST /api/surface {surface:"routing"}` 片段补丁不暴露
  （不是图形编辑器，且提交依赖块边界换图/热重配 M3b/T52），不假成功；
- **降级诚实**：rt-host 不可达 / `host_link` 断开或状态过期 → 警示条 + 只显示最后成功快照；
  `/api/channels` 501 `not_wired` → 整块 NOT_AVAILABLE + 稳定码 + 任务号 T55；历史不可用 → 清空历史并提示；
- 与 `/channels` 页映射口径一致（lane / 0-based DAC / 1-based MOTU 物理输出 + 已知位置 L/R/SL/SR/TF/TFR/LFE，
  未知位置显示 OUT n）；编辑入口不重复，页面提供跳转。

契约 mock（复用现有 fixture，**无新增后端端点/fixture**）：

- `/routing` 的数据来自现有 mock 的 `/api/status`（present | stale | missing）+ `/api/channels`
  （wired | degraded | unavailable）+ `/api/plan/history`；浏览器 QA 用 `POST /__mock/status-fixture` 与
  `POST /__mock/channels-fixture` 组出正常/降级/未接线三态（`artifacts/qa/qa-routing.mjs`，DEV-ONLY）；
- mock 的 degraded 声道 fixture 把 `mapping` 上报为 `null`（与真实 daemon 同形）、`reasons.mapping` 带原因；
  history 保持 T54 mock 契约形态；路由页解析同时覆盖真实 daemon history 形状（单测双向断言）。

### M5：校准页 + 8 步向导（本任务 T37，P1 离线闭环）

页面与路由：

| 路由 | 名称 | 页面 |
|---|---|---|
| `/calibration` | calibration | 校准列表：job 表格（分页/状态/verdict/时间/行内取消）+ 新建向导入口 + 最近 job / 向导状态 / 测量上传摘要；无数据如实空态 |
| `/calibration/wizard` | calibration-wizard | Dirac 式 8 步向导（stepper + 主面板 + 门禁卡 + 底部固定操作条；`?step=1..8` 可刷新恢复） |
| `/calibration/jobs/:id` | calibration-job | job 详情：状态时间线 / 产物（下载）/ verdict + 指标 + 1/3-oct 表 / before-after 应用回滚 |

数据链路（页面唯一数据源，不读假数据）：

```
src/api/calibration.ts    校准 REST 类型 + 运行时校验（schema/枚举/结构漂移 -> invalid_response）
                          + 端点封装（listJobs/getJob/createJob/cancel/apply/rollback/upload/wizard/state/apply）
                          + gate 纯函数（canAdvance/maxUnlockedStep/stepDisplayState…）+ 错误码归类 + TruthTag 判定
src/utils/wav.ts          WAV 头解析（RIFF/fmt/data）、48k/16-24bit/1-2ch 格式校验、baseline/applied 结构对齐
src/stores/calibration.ts wizard 配置/state + job 列表 + 原始字节上传 + job 真实状态轮询（终态即停/隐藏暂停）
                          + apply/rollback 回执；可注入时钟/定时器/api 的纯逻辑控制器（单测覆盖）
src/components/calibration/
                          WizardStepper / GateCard / JobTimeline / VerdictBadge / UploadCard /
                          JobResultPanel（verdict+指标+频段表+设计+产物）/ ApplyPanel（before-after+二次确认+回执+导出）
src/views/CalibrationView.vue / CalibrationWizardView.vue / CalibrationJobView.vue
```

8 步与门禁（PLAN §10.2 逐条落地）：

| # | 步骤 | 数据来源 | 前端行为 | 当前状态 |
|---|---|---|---|---|
| 1 | 设备检查 | `/api/status` device/format/host_link | 只读事实 + TruthTag；门禁由后端判定 | 真（M2） |
| 2 | 布局与声道 | `/api/status` 只读 + 后端 gate | 只读展示格式/声道；**NOT_AVAILABLE + M4**，不伪造映射 | 灰态（M4） |
| 3 | 麦克风与校准设置 | `GET/PUT /api/calibration/wizard` | 校准文件上传（服务端 SHA-256）、朝向、目标曲线、centers/tolerance/min_bands；保存后由后端 gate 判定 | 真 |
| 4 | 测点 | 后端 gate | 单点（皇帝位）为唯一方案；**多点/ART 灰态 NOT_AVAILABLE + P3** | 单点真；多点/ART 灰态 |
| 5 | 测量 | `POST /uploads` + 本地 WAV 校验 | 离线两段 WAV（48k/16-24bit/1-2ch/对齐校验）；**实时测量灰态 NOT_AVAILABLE + T42/P2** | 离线真；实时灰态 |
| 6 | 分析 | `POST /jobs` + `GET /jobs/{id}` 轮询 | 创建 job → 真实步骤时间线 → verdict/指标/频段表/产物；失败显示稳定 code + 重试/取消 | 真（后端 job 未接时 503 not_wired） |
| 7 | 设计 | job `result.design/plan` | design 残留 FULL/PARTIAL/NONE + 准入结果 + plan 摘要；无产物被 gate 阻止 | 真 |
| 8 | 回顾/应用/回滚 | `/jobs/{id}/apply|rollback` 或 `/wizard/apply` | before/after + 快照 + **二次确认弹窗** + 回读（版本/graph_id/sha/ack actual|synthetic）+ 导出配置 JSON | 真 |

关键纪律：

- **gate 只认后端** `GET /api/calibration/wizard/state` 的 `steps[].status`（passed / blocked / not_available）；
  缺失/畸形 steps 一律按 `invalid_response` 拒绝并 fail closed（下一步禁用），前端不自行判过；
- **not_available 是声明式能力缺口**（不是用户可修复的门禁）：步骤灰态 + 原因 + 任务号，不产生任何数据、可继续；
  blocked 则禁用「下一步」并显示原因；
- 所有进度来自真实 job 状态轮询（1.5s，终态即停、页面隐藏暂停），**没有定时器假进度条**；
- 数值/图表全部带 TruthTag：job 引擎为 `real_audio` 才是「实测」，其它/缺失按「示意/未接入」显示；
- 刷新恢复：wizard 配置 + gate state + 最近 job（active 或 last）全部来自后端，`?step=` 记住当前步；
  第 5 步两段上传通过 `PUT /wizard` 的可选 `measurement` 草稿持久化（刷新后可恢复）；
- 401 交给共享 ApiClient（会话 store 清态跳登录）；后端未接入（503 not_wired / available=false）如实显示「未接入」。

### 系统信息页 + 日志页（本任务 T47，M6 提前）

背景（必须在页面上如实区分）：当前 N150 部署 release **v0.1.2**，但 /healthz 曾上报 `version=0.1.0`（crate 版本）。
系统页把 **crate_version（编译进二进制的 Cargo 版本）/ release_version（部署发布版本）/ commit** 分开展示，
crate ≠ release 时页面顶部红色警示条醒目提示（不把任何一项当成"版本"糊弄过去）。

路由与页面：

| 路由 | 名称 | 页面 |
|---|---|---|
| `/system` | system | 版本与构建 / 主机 / 服务 / 设备 / 控制通道 / 重启 rt-host 六张卡 + 修改密码 + 内嵌日志查看器 + 诊断下载 |
| `/system/logs` | logs | 独立日志页（复用日志查看器全页） |

数据链路（页面唯一数据源，不读假数据）：

```
src/api/system.ts         /api/system/* 类型 + 运行时校验（schema/类型漂移 -> invalid_response）
                          + 版本一致性（crate/release）、MOTU 设备识别、重启能力、错误归类
                          + Content-Disposition 文件名解析、改密表单校验等纯函数
src/stores/system.ts      info 控制器（加载/刷新/并发去重/失败保留旧数据）
                          + 日志控制器（unit/行数切换、自动刷新 5s、页面隐藏暂停、切换清空旧数据）
src/stores/version.ts     release 版本（匿名 GET /healthz；顶栏/侧栏/登录页页脚统一使用，未知显示「—」）
src/components/system/    SystemLogsPanel（unit/行数/自动刷新/复制/不可用原因）
                          PasswordChangeCard（旧/新/确认，成功后重新登录）
                          RestartCard（能力未允许 -> NOT_AVAILABLE + 原因；允许 -> 输入确认词二次确认）
src/views/SystemView.vue / SystemLogsView.vue
```

关键纪律：

- **版本**：顶栏、侧栏 brand、登录页页脚统一显示 **release 版本**（来自匿名 /healthz）；未知时显示「—」，
  绝不拿 webui package 版本或 crate 版本冒充；系统页展示 crate/release/commit 三项并标出一致性；
- **不禁用/不隐藏真实信息**：daemon / rt-host 的 active / enabled / PID 全部来自 daemon；命令失败如实 null + 原因；
- **设备**：/proc/asound 列表原样展示，MOTU（card_id/model/driver 命中）打 MOTU 标签；空列表给原因；
- **日志**：unit 切换（daemon / rt-host）、行数 100/200/500、自动刷新开关（默认关，5s，隐藏暂停）、复制；
  journalctl 不可用（503 `logs_unavailable`）显示 NOT_AVAILABLE + 后端原因，不伪造日志；
- **诊断**：`GET /api/system/diagnostics` 原始 JSON 下载，文件名来自 Content-Disposition（版本 + UTC 时间），
  无 Content-Disposition 时前端回退 `khaen-diagnostics-v<release>-<UTC>.json`；诊断内容不含密码/CSRF/cookie；
- **修改密码**：旧/新/确认本地校验 -> `POST /api/system/password`；成功后强制重探会话、登出清 Cookie 并回登录页
  （登录页提示「密码已修改」），避免浏览器仍持新会话导致"假在线"；
- **重启 rt-host**：仅在 `info.restart.allowed=true` 时显示按钮；弹窗要求输入确认词 `restart` 才提交
  （二次确认由前端负责）；未启用显示 NOT_AVAILABLE + 原因；daemon 自身重启不实现（T46 返回 501），页面明示；
- 401 交给共享 ApiClient（会话 store 清态跳登录）；错误保留最后一次成功数据并提示；
- 顶栏「修改密码」菜单从 M6 占位改为直达 `/system#password`；概览/全局骨架提示条更新为当前真实接入状态。

### 明确未接入（不展示假数据）

- **路由编辑/重排** -> 只读路由矩阵页已接入（T58：`/routing`，输入 → 矩阵域 → DAC 输出诊断）；
  写操作（交叉点编辑/lane 重排/应用路由）需要 PlanGraph 编辑器/designer（T59），页面 NOT_AVAILABLE + 原因；
  控制面低层 `POST /api/surface {surface:"routing"}` 片段补丁不是图形编辑器，本页不暴露（不假成功）；
- **主音量 / 当前逐 lane 增益回读** -> 控制协议无对应 op（T70f-a/b），音量页 NOT_AVAILABLE + 原因 + 任务号；
- **工作模式 analog（模拟/本地输入）** -> 控制面只接通 off/aes67（analog/toslink/dante 未实现），页面禁用并标注 T70f/T70g；
- **声道热重配 commit** -> M3b 热重配状态机（T52）与 daemon M4 接口（T55）未完成，页面 prepare 保留、commit 501 如实显示；
- **M4 后端（/api/channels、/api/plan/*）** -> T55 并行实现；前端用契约 mock + 单测开发，未接通时整块 NOT_AVAILABLE；
- daemon 自身重启、实时测量、多点/ART -> 后端能力缺口（T46 / T42 / P3），页面/向导内明示 NOT_AVAILABLE + 原因；
- 真实 Rust 后端联调（T70f-b/T55/T52 合并后）-> 由 lead 执行；本任务用契约 mock + 单测 + 浏览器断言验证前端行为。

## `GET /api/status` 契约（PLAN §4.4，khaen-status/v1）

需要登录（401 走统一会话处理）。响应体：

```json
{
  "schema": "khaen-status/v1",
  "daemon": {"version": "0.1.1", "uptime_s": 123, "bind": "0.0.0.0:80"},
  "host_link": {"connected": true, "since": "RFC3339", "socket": "/run/khaen/rt-host.sock", "status_age_ms": 120},
  "state": {"value": "running", "muted": false, "safe": false, "last_error": null},
  "plan": {"graph_id": "…", "sha256": "…", "version": 3, "latency_samples": 1023, "applied_at": "RFC3339", "source": "upload"},
  "format": {"plan_rate": 48000, "device_rate": 48000, "rate_asserted": true, "block_frames": 128, "channels": 22,
             "encoding": "L24", "aes67": {"channels": 2, "map": [0, 1], "pt": 97, "group": "239.69.67.1", "port": 5004}},
  "input": {"packets": 895912, "underruns": 21, "jitter_fill": 880, "ssrc": 534361700, "queue_full_drops": 0, "recv_errors": 0},
  "output": {"blocks": 352000, "xruns": 0, "out_errors": 0, "bytes_written": 2973696000},
  "device": {"id": "MOTU", "card": "hw:CARD=…", "state": "present", "rate_readback": 48000},
  "calibration": {"active_job": null, "last_job_id": "…", "last_verdict": "PASS"},
  "not_wired": [{"path": "/api/surfaces", "reason": "…"}],
  "reasons": {"device_rate": "rt-host 未回报"}
}
```

前端约定：

- 缺失值一律 `null`，原因放 `reasons`（支持嵌套路径 `format.device_rate` 或扁平字段名 `device_rate`）；
  整个子系统缺失时该组可为 null/缺省，前端按全 null 组归一化并显示「未接入」；
- schema 不是 `khaen-status/v1`、字段类型漂移、`not_wired`/`reasons` 结构非法 -> 前端拒绝显示（错误 + 重试），不把畸形响应当成数据；
- 过期判定：`host_link.connected === false` 或 `status_age_ms > 2000`；
- 未知的 `state.value` 原样显示（不翻译、不猜测），已知值映射为本地化文案。

## 校准 REST 契约（PLAN §4.7 / §10.4，任务 T37 前端消费）

后端由并行任务 T36 实现（本仓库 crates/daemon）；下列形状是前端 mock/校验所依据的契约
（T36 已确认大部分字段；集成时若有差异由 lead 对齐）。所有端点需要会话，写操作带 `X-KHAEN-CSRF`；
错误体固定 `{"error":{"code","message","details"}}`；时间 RFC3339。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/calibration/jobs?limit&offset` | `{schema:"khaen-calibration-jobs/v1", available, reason, engine, active_job_id, jobs[], total, limit, offset}`；后端未接入时 200 + `available:false` + reason |
| POST | `/api/calibration/jobs` | `{kind:"single_point_offline", inputs:{plan_sha,baseline_upload_id,applied_upload_id,centers_hz,tolerance_db,min_bands}}` → 202 `{job_id}`；同一时刻一个 active job，冲突 409 `switch_busy` |
| GET | `/api/calibration/jobs/{id}` | `{schema:"khaen-calibration-job/v1", job:{id,kind,status,verdict,created_at,started_at,finished_at,duration_ms,engine,apply_state,applied_version,steps[],artifacts[],inputs,result,error,reason}, reasons}` |
| POST | `/api/calibration/jobs/{id}/cancel` | `{job_id,status}`；非 active 409 `job_not_active` |
| POST | `/api/calibration/jobs/{id}/apply` | `{applied:true, job_id, applied_version, graph_id, sha256, snapshot, ack:{synthetic,actual}, readback, applied_at}` |
| POST | `/api/calibration/jobs/{id}/rollback` | `{rolled_back:true, restored_version, graph_id, sha256, readback, rolled_back_at}` |
| POST | `/api/calibration/uploads` | 原始字节 body（`application/octet-stream` 或 `audio/wav`；≤ 64 MiB）→ 201/200 `{upload_id(==sha256), sha256, bytes, created}` |
| GET | `/api/calibration/jobs/{id}/artifacts/{name}` | 产物下载（浏览器直链；daemon 侧按 manifest sha256 校验） |
| GET/PUT | `/api/calibration/wizard` | `{schema:"khaen-calibration-wizard/v1", config:{mic_cal_sha256,mic_cal_filename,orientation:"0deg"|"90deg",target_mode:"flat"|"tilt10",single_point:{centers_hz,tolerance_db,min_bands}}, defaults, saved, updated_at}`；PUT 可带可选 `measurement:{baseline_upload_id,applied_upload_id,plan_sha}` 草稿 |
| GET | `/api/calibration/wizard/state` | `{schema:"khaen-calibration-wizard-state/v1", current_step, steps:[{step,key,status:"passed"|"blocked"|"not_available",reason,task}], active_job_id, last_job_id, measurement, snapshot, updated_at, reasons}`；steps 必须完整覆盖 8 步 |
| POST | `/api/calibration/wizard/apply` | 应用最近成功 job 的 plan（走 prepare/commit），返回同 job apply |

稳定错误码（前端映射本地化文案，见 `src/api/calibration.ts` classifyCalibrationError）：
`not_wired`(503) / `switch_busy`(409) / `job_not_found` / `upload_not_found` / `artifact_not_found`(404) /
`invalid_request`(400) / `invalid_upload` / `unsupported_format`(415) / `upload_too_large`(413) /
`job_not_succeeded`(409) / `plan_unavailable`(409) / `rollback_unavailable`(409) /
`artifact_sha_mismatch`(409) / `wizard_not_saved`(409)。

## 系统信息 REST 契约（任务 T47 前端消费；后端 T46 并行实现）

需要登录（401 走统一会话处理）；写操作带 `X-KHAEN-CSRF`；错误体固定 `{"error":{"code","message","details"}}`。
T46 契约里 **/healthz 的 version 改为 release 版本**（crate 版本只出现在 `/api/system/info.crate_version`）。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/healthz` | **匿名**；`{ok, version, uptime_s}`，version = release 版本（缺失时回退 crate 版本）；前端用它统一顶栏/页脚版本 |
| GET | `/api/system/info` | `{crate_version, release_version, commit, build_profile, rustc, target, bind, state_dir, rt_status_file, uptime_s, hostname, kernel, os, services:{daemon,rt_host:{active,enabled,pid,reason}}, devices:[{card_id,driver,model,channels,rates}], host_link:{connected,since,socket,status_age_ms}, restart:{allowed,reason,target,last_restarted_at}, reasons}` |
| GET | `/api/system/logs?unit=daemon\|rt-host&lines=N` | `{schema:"khaen-system-logs/v1", unit, unit_name, requested_lines, lines:[string], truncated, source, reason}`；N ∈ [1,500]；journalctl 不可用 -> 503 `logs_unavailable` + reason |
| GET | `/api/system/diagnostics` | JSON 下载（Content-Disposition 文件名带版本 + UTC 时间）；合并 status+info+设备+最近日志摘要，**不含密码/CSRF/cookie** |
| POST | `/api/system/password` | `{old,new}`（登录 + CSRF；复用 auth password 语义）；失败 401 `invalid_credentials`（旧密码错）/400 `invalid_password` |
| POST | `/api/system/restart` | `{target:"rt-host"}`（登录 + CSRF）；仅 `KHAEN_ALLOW_RESTART=1` 时允许，未启用 403 `restart_disabled` + reason；daemon 自身重启不实现（501） |

字段约定：

- 缺失值一律 `null`，原因放 `reasons`（字段路径 -> 原因，兼容末段扁平键）；
- 服务/设备字段的采集失败（systemctl / /proc 不可用）如实 `null` + reason，**不 500、不编造**；
- `restart.allowed` 缺失/未知一律按 false 处理（fail closed：不显示危险操作）；
- schema 字段可选（T46 可后补），存在但不是 `khaen-system-info/v1` / `khaen-system-logs/v1` 时前端按 invalid_response 拒绝；
- 版本判定：`normalizeVersion` 去尾部空白与前导 v 后严格相等 -> consistent，否则 mismatch；任一缺失 -> unknown
  （页面不再编造「一致」）。

稳定错误码（前端映射本地化文案，见 `src/api/system.ts` classifySystemError）：
`logs_unavailable`(503) / `restart_disabled`(403) / `invalid_request`(400) / `invalid_password`(400) /
`invalid_credentials`(401) / `not_wired`(503) / `not_implemented`(501) / `csrf_invalid`(403)。

## M3/M4 控制面契约要点（T53/T54 前端消费）

- **两阶段**：所有写操作 prepare → commit/abort；prepare 只返回草稿（`engine_token`/planned version/预览），commit 才生效；
- **错误体**：`{"error":{"code","message","details"}}`；`host_link_unavailable`(503) / `unsupported`(501) / `not_wired`(501/503)
  / `engine_token_invalid`(409) / `no_base_plan`(409) / `admission_rejected`(409) / `sha_mismatch`(409) /
  `upload_too_large`(413) / `invalid_plan`(400) / `channel_layout_unavailable`(409) / `host_link_unsupported`(501)；
- **不假成功**：未接通的 op 返回 501/503 + reason，页面显示 NOT_AVAILABLE + 原因 + 任务号（T55 / T52 / T70f）；
  控制面回执与设备回读分开显示（format commit 需要重启 rt-host，页面不把控制面回执当设备已生效）。

## 认证契约（PLAN §4.2）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/auth/status` | 匿名；`{configured, authenticated}` |
| POST | `/api/auth/setup` | `{password, confirm}` -> 201 `{csrf, expires_in}` + Set-Cookie；已配置 409 |
| POST | `/api/auth/login` | `{password}` -> 200 `{csrf, expires_in}` + Set-Cookie；失败 401，5 次/60s 后 429 |
| GET | `/api/auth/session` | `{authenticated, csrf, expires_at}`；未登录 401 |
| POST | `/api/auth/logout` | 清会话（幂等） |
| POST | `/api/auth/password` | `{old, new}`（登录 + CSRF） |

Cookie：`HttpOnly; SameSite=Strict; Path=/`（前端不读）；写请求带 `X-KHAEN-CSRF: <csrf>`；
错误体固定 `{"error":{"code","message","details"}}`（前端把稳定 code 映射成本地化文案，后端 message 只作详情）。

### 契约 mock（仅开发/测试，不进 dist）

```bash
# 终端 1：启动 mock（内存态；重启即回到未初始化，可反复演练 setup）
node scripts/mock-auth-server.mjs --port 8790
node scripts/mock-auth-server.mjs --port 8790 --status-fixture stale   # present | stale | missing

# 终端 2：vite dev，/api 指向 mock
KHAEN_DEV_API=http://127.0.0.1:8790 pnpm dev
# Windows PowerShell: $env:KHAEN_DEV_API='http://127.0.0.1:8790'; pnpm dev
```

`/api/status` 三种 fixture（`scripts/status-fixtures.mjs`，固定时间戳、可复现）：

| fixture | 形态 | 用途 |
|---|---|---|
| `present` | 全字段真实值，host_link 新鲜（120ms） | 正常概览/截图 |
| `stale` | `connected=false` + `status_age_ms=15000` + `state=faulted` | 黄色 stale 标识 + 错误展示 |
| `missing` | 每个叶子 null + `reasons` + `not_wired` | 逐字段「未接入」TruthTag |

校准 REST 同样有契约 mock（`scripts/calibration-fixtures.mjs`，DEV/TEST ONLY，不进 dist）：

```bash
node scripts/mock-auth-server.mjs --port 8790 --calibration-fixture success
#   empty | success | failed | unavailable
node scripts/mock-auth-server.mjs --port 8790 --system-fixture degraded
#   mismatch | consistent | degraded
node scripts/mock-auth-server.mjs --port 8790 --control-fixture wired --channels-fixture wired
#   control:  wired | unavailable          （默认 wired：静音/音量/模式/格式两阶段）
#   channels: wired | degraded | unavailable
```

不确定/缺省时 mock 按 `status-fixture=present`、`calibration-fixture=empty`、`system-fixture=mismatch`、
`control-fixture=wired`、`channels-fixture=wired` 启动；QA 截图使用 wired 形态（未接入形态另有截图）。

| fixture | 形态 | 用途 |
|---|---|---|
| `empty` | 无配置/无 job/无上传；step1 通过、step3 blocked | 从第 1 步开始演练门禁 |
| `success` | 已保存配置 + 两段测量上传 + 一个 succeeded/PASS 离线 job（apply 未做） | 列表/第 6–8 步/应用回滚演示 |
| `failed` | 配置已保存 + 一个 failed job（稳定码 `E_CLOSED_LOOP_DELTA`） | 失败态/重试演示 |
| `unavailable` | 模拟 daemon NotWired：`GET /jobs` 200 available:false；job/apply/artifact 503 not_wired；wizard 真实部分可用 | 「未接入」诚实展示 |

mock 的 job 引擎固定为 `synthetic`（前端据此打「示意」TruthTag，不冒充实测）；job 状态在每次
`GET /jobs/{id}` 时推进一步（真实 daemon 由 worker 推进），测试可用 `mock.calibration.completeJob/failJob/tickJob`。
运行时切换：`POST /__mock/calibration-fixture {fixture}`（dev-only）。

系统信息同样有契约 mock（`scripts/system-fixtures.mjs`，DEV/TEST ONLY，不进 dist）：

| fixture | 形态 | 用途 |
|---|---|---|
| `mismatch`（默认） | **缺陷实例**：crate_version=0.1.0 / release_version=v0.1.2 / commit=855f4c4；daemon 运行、rt-host 未运行（pid=null+reason）；MOTU+板载两张卡；restart 允许 | 版本不一致醒目警示、正常系统页/截图、重启演练 |
| `consistent` | crate=release=0.1.2；daemon/rt-host 均在运行（PID 4211/4247）；host_link 新鲜 | 演练「版本一致」形态（无警示条） |
| `degraded` | systemctl / /proc 全部不可用：字段 null + reasons（20 项）、设备空、日志 503 `logs_unavailable`、restart 不允许 | 未接入/降级诚实展示 |

运行时切换：`GET /__mock/system` / `POST /__mock/system-fixture {fixture}`（dev-only；切换同时清空 mock 的重启状态）。
mock 的 `/healthz` 返回 **release 版本**（mismatch/consistent 为 v0.1.2；degraded 回退 crate 版本 0.1.0）。

音量/格式/模式（M3）契约 mock（`scripts/control-fixtures.mjs`，DEV/TEST ONLY，不进 dist）：

| fixture | 形态 | 用途 |
|---|---|---|
| `wired`（默认） | 22 lane 增益、muted=false、mode=off、48 kHz/128；prepare/commit/abort/静音/设备重试全部走真实两阶段状态机；/api/status 回读 mute/format/plan | 音量/格式/模式正常路径、两阶段回执、截图 |
| `unavailable` | 控制面 503 `host_link_unavailable`（rt-host 不可达）；/api/status 仍可读但 mute/format null | 「未接入/后端子任务未完成」诚实展示 |

运行时切换：`GET /__mock/control` / `POST /__mock/control-fixture {fixture}`（dev-only；切换清空待提交草稿）。
单草稿槽语义与真实 daemon 一致：`prepare` 后再 `prepare` 返回 409 `admission_rejected`；`abort` 后 token 失效。

方案与声道（M4）契约 mock（`scripts/channels-fixtures.mjs`，DEV/TEST ONLY，不进 dist）：

| fixture | 形态 | 用途 |
|---|---|---|
| `wired`（默认） | `khaen-channels/v1` 22 输入/22 输出 + MOTU 映射 + AES67 输入 + 真实 `plan` 段（routing_known=true）；plan upload/history/inspect/prepare 可用；**channels commit 返回 501 `host_link_unsupported`**（M3b/T52 未完成，与真实后端同形） | 方案摘要/映射表/上传/历史回滚/热重配 prepare + 诚实 501 |
| `partial` | **真实部分路由形态**（T60）：4 DAC，lane0→dac0、lane1→dac2，`dac_map=[0,null,1,null]`；mapping 非空 | 覆盖 `dac_map` 的 null 项（该 DAC 静音/无 lane）不被误判 invalid_response |
| `degraded` | 部分字段 null + reasons（dac_map/input.sdp_sha256 缺失、`mapping:null`=routing 未知、plan.routing_known=false） | 逐字段「未接入」+ 不编映射/history |
| `unavailable` | 全部 503 `not_wired`（T55 未实现形态） | 整块 NOT_AVAILABLE + 任务号 |

运行时切换：`GET /__mock/channels` / `POST /__mock/channels-fixture {fixture}`（dev-only）。
方案上传使用真实字节流（`POST /api/plan/upload` 原始 body，≤ 4 MiB），mock 返回 daemon 真实回执形态
（`segments` 区段名数组 + `segment_count` + `stored_path`，首次 201/幂等 200，无 upload_id/version）；
`POST /api/plan/inspect {plan_path,plan_sha256?}` 返回 plan+compile 摘要（GET 为 405，未知 plan_path 为 400）；
`POST /api/prepare {plan_path,plan_sha256}` 读落盘路径准备草稿；
`GET /api/plan/history` 是 daemon 真实形态（`current:{version,graph_id}` + `entries[]`，含 v5..v7）；
`rollback` 生成新版本草稿（需 commit，mock 会验证）。
/dev-only 端点 `GET /__mock/control`、`/__mock/channels` 返回当前 fixture 与服务端状态，便于 QA 断言与 fixture 切换。

mock 只实现 PLAN §4.1/§4.2/§4.4 与 M3/M4 控制面契约（M3b/T58 路由页不新增端点：复用上述 status/channels/history fixture 组合），
用于前端自测，**不能替代真实 Rust/axum 后端**。

## 构建与 dist 入库

`webui/dist/` 是**提交进仓库**的构建产物：daemon 在编译期把它整个编进二进制，
所以没有 Node 工具链的人也能直接 `cargo build` 出带控制台的 daemon。

```bash
cd webui
pnpm install --offline --frozen-lockfile   # 首次或依赖变更（提交 pnpm-lock.yaml）
pnpm build                                 # 产物写入 webui/dist/
```

纪律：**改完 `src/`、`public/`、`index.html` 或 `vite.config.ts`，必须重新 `pnpm build` 并一起提交 `dist/`。**
构建必须**确定性**：构建信息只来自 `KHAEN_WEBUI_BUILD_ID`（缺省 `local`），不使用任何时间戳；
连续两次 `pnpm build` 的 dist 逐文件 SHA256 必须一致（本任务已校验：7 个文件全部一致）。

## 门禁（提交前必须全绿）

```bash
cd webui
pnpm test                      # Node 24 内置 test runner（.ts 直跑，无需额外依赖）
pnpm typecheck                 # vue-tsc --noEmit
node scripts/i18n-check.mjs    # 键集对等 + 无硬编码中文
pnpm build                     # 产物写 dist/（连续两次哈希一致）
```

## 单测 / mock 覆盖（`tests/`）

```
tests/client.test.ts             fetch 封装：credentials、CSRF、错误体、401 回调、API 版本、超时/网络错误
tests/auth-api.test.ts           /api/auth/* 端点方法/请求体/CSRF 开关/响应校验
tests/session.test.ts            会话 store：ensureSession 两段探测、缓存/并发去重、登录/初始化/登出、401 清态跳转、过期
tests/guard.test.ts              路由守卫：next 防开放重定向、登录回跳、fail closed
tests/validation.test.ts         表单本地校验：必填/最短/两次一致/边界
tests/errors.test.ts             后端错误码 -> 归一化类别 + 本地化文案键
tests/mock-integration.test.ts   契约 mock 端到端：认证全流程 + /api/status 三种 fixture（present/stale/missing）+ 401
tests/status-api.test.ts         /api/status 运行时校验、派生逻辑（stale/state/欠载率/uptime/reasons）、错误归类、401 传递
tests/status-polling.test.ts     5s 轮询控制器：立即拉取、定时刷新、并发去重、隐藏暂停/恢复、错误保留旧数据、401、停止/重启
tests/api-contract.test.ts        中央合同：解析 docs/API.md 端点表，断言 webui 使用的 path/method 全部在合同中
tests/navigation.test.ts          导航/路由：侧栏项↔route record↔双语言标题↔requiresAuth 一致性
tests/meta-api.test.ts           能力快照：/api/meta 解析（数组/对象 surfaces、not_wired op/surface、三态）
tests/meta-store.test.ts          能力 store：缓存/并发去重/失败保留旧快照/unknown 降级
tests/control-api.test.ts         （T62 扩展）surface routing/layout 请求体、changed/diff 可选字段、validators
tests/mode-api.test.ts / mode-store.test.ts       Mode 页：surface mode 两阶段 body/commit/abort、401/503/409、别名与写门禁
tests/layout-api.test.ts / layout-draft.test.ts   Layout 页：surface layout 两阶段 body、行增删/校验/草稿 patch
tests/motu-api.test.ts / motu-store.test.ts / motu-mock.test.ts / volume-motu.test.ts  MOTU：GET/PUT 契约、轮询、prepare/commit、错误码、无 abort 本地取消
tests/routing-api.test.ts / routing-store.test.ts / routing-mock.test.ts  路由可编辑：surface routing 精确 body、mapping 当前值/降级、commit/abort