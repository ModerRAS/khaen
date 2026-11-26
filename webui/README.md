# khaen webui

khaen DSP 引擎的 Web 控制台：Vue 3 + TypeScript + Element Plus + Vite + vue-router + vue-i18n。
由 khaen-daemon 同源托管（编译期 `include_bytes!` 内嵌 `webui/dist`），运行期不需要 Node / Python / 反代。

> 规划：`HANDOFF/20261126-khaen-webui-plan/PLAN.md`（仓库外）§4 API、§5 路由、§6.1 登录、§6.2 概览、§11 UI 布局、§12 主题与 i18n。
> 当前里程碑：**M3（音量/格式/模式，任务 T53）+ M4（方案与声道，任务 T54）**；M2/M5/M6 已落地。
> 概览页（M2）接 `GET /api/status`；校准页/向导（M5）接 `/api/calibration/*`；系统信息页/日志页接 `/api/system/*`；
> 音量/格式/工作模式页接 `/api/mute`、`/api/surface`、`/api/format`（prepare → commit 两阶段，T70f-b 代理到 rt-host）；
> 方案与声道页接 `/api/channels`、`/api/plan/*`、`/api/prepare|commit|abort|rollback`（M4 后端 T55 / 热重配 T52 仍在并行实现）。
> 真实 Rust 后端联调与真机热切换由 lead 在合并后执行（本任务 NOT_RUN，前端用契约 mock + 单测 + 浏览器断言验证）。

## 当前状态

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
src/api/channels.ts        khaen-channels/v1 校验 + 方案上传/历史/预览解析（兼容控制面 inspect 嵌套形态）+ MOTU 标签纯函数
                          + 客户端 WebCrypto sha256 + 声道补丁校验 + 错误归类
src/stores/channels.ts     声道读取控制器 + 方案上传/预览/两阶段（prepare→commit/abort）+ 回滚草稿 + 历史；单写操作互斥
src/views/ChannelsView.vue 方案摘要 / 输出映射表 / AES67 输入 / 上传卡 / 历史回滚表 / 热重配卡 / 两阶段草稿回执
```

契约（T54/T55 任务契约；后端 T55 并行实现，前端按契约 mock + 单测开发）：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/channels` | `khaen-channels/v1`：`{output:{channels,dac_map}, input:{channels,map,source,sdp_sha256}, mapping:[{lane,dac}], reasons, not_wired}`；缺失字段 null + reason |
| PUT | `/api/channels` | 两阶段 prepare：校验并返回 `{engine_token,planned_version,changed,preview{old,new,audio_gap_ms_estimate},warnings,blockers}`；commit 走 `POST /api/commit` |
| POST | `/api/plan/upload` | 原始 plan 字节（≤ 4 MiB）→ `{upload_id,sha256,graph_id,version,segments,latency_samples}`；同 sha 幂等 |
| POST | `/api/prepare` | `{upload_id,sha256}` → 方案草稿（engine_token/planned_version/graph_id） |
| GET | `/api/plan/history` | `{schema,current_version,versions:[{version,graph_id,sha256,applied_at,source}]}`（前端兼容数组/versions/history/items 形态） |
| GET | `/api/plan/inspect` | 只读预览（plan {block_size,sample_rate,in_channels,lanes,dac_channels} + admission + compile）；不可用时页面降级为上传摘要 |
| POST | `/api/rollback` | `{target?}` → 回滚草稿（rolled_back_to；版本号递增，历史不改写），仍需 commit |
| POST | `/api/commit` / `/api/abort` | 共享两阶段提交/丢弃（channels 草稿在 M3b/T52 完成前 commit 返回 501 `host_link_unsupported`） |

诚实边界（页面逐条明示）：

- **M4 后端未接入（T55）**：/api/channels 与 plan upload/history/inspect/rollback 返回 501 `not_wired` 时，页面整块
  NOT_AVAILABLE + 稳定码 + 原因 + 任务号（T55），不编造映射/历史；
- **热重配未完成（M3b/T52）**：PUT prepare 可返回 preview/warnings/blockers，但 commit 会被后端以 501
  `host_link_unsupported` 拒绝；页面如实显示错误并写明任务号，绝不显示成功；变更前警告「会中断出声」（预计 1–2 s）；
- **sha256 双侧校验**：上传前浏览器 WebCrypto 计算，上传后与服务端返回比对；不一致即拒绝 prepare（不信任任何一侧）；
- **MOTU 标签只标已知位置**：1-2 L/R、3-4 SL/SR、5-6 TF/TFR、8 LFE；未知位置显示 `OUT n`，不猜；
- **历史/预览不编造**：/api/plan/history 不可用时清空历史并显示原因；inspect 不可用时只显示上传摘要；
- 回滚 = 版本号递增的新提交（历史保留），必须 commit 才生效；提交/回滚均有二次确认。

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

- **路由矩阵 `/routing`** -> 菜单灰显并标注 M3（本任务 T53 未包含路由页）；
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
| `wired`（默认） | `khaen-channels/v1` 22 输入/22 输出 + MOTU 映射 + AES67 输入；plan upload/history/inspect/prepare 可用；**channels commit 返回 501 `host_link_unsupported`**（M3b/T52 未完成，与真实后端同形） | 方案摘要/映射表/上传/历史回滚/热重配 prepare + 诚实 501 |
| `degraded` | 部分字段 null + reasons（dac_map/input.sdp_sha256 缺失等），history 可用 | 逐字段「未接入」+ 不编映射 |
| `unavailable` | 全部 503 `not_wired`（T55 未合并） | 整块 NOT_AVAILABLE + 任务号 |

运行时切换：`GET /__mock/channels` / `POST /__mock/channels-fixture {fixture}`（dev-only）。
方案上传使用真实字节流（`POST /api/plan/upload` 原始 body，≤ 4 MiB），mock 返回 `sha256/graph_id/version`；
`inspect` 返回 plan+compile 摘要；`history` 含 v5..v7 三条记录；`rollback` 生成新版本草稿（需 commit，mock 会验证）。
/dev-only 端点 `GET /__mock/control`、`/__mock/channels` 返回当前 fixture 与服务端状态，便于 QA 断言与 fixture 切换。

mock 只实现 PLAN §4.1/§4.2/§4.4 与 M3/M4 控制面契约，用于前端自测，**不能替代真实 Rust/axum 后端**。

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
tests/format.test.ts             Intl 数字/时间、id 省略、字节/采样率单位换算
tests/wav.test.ts                WAV 头解析/格式校验（48k/16-24bit/1-2ch）/baseline-applied 对齐/截断拒绝
tests/calibration-api.test.ts    校准 REST 响应运行时校验（schema/枚举/8 步 steps 完整性/apply 回执）+ 端点请求
                                 + gate 纯函数（fail closed/not_available 可跳过）+ 错误码归类 + 原始字节上传/CSRF
tests/calibration-store.test.ts  wizard 加载/保存/上传草稿持久化 + job 轮询状态机（终态停止/网络重试/404 停止/
                                 隐藏暂停）+ apply/rollback 回执 + 503 not_wired -> unavailable
tests/calibration-mock.test.ts   契约 mock 端到端：鉴权/CSRF、上传（幂等/类型/超限 413）、wizard GET/PUT/刷新恢复、
                                 job 创建->轮询->成功->产物 sha->apply->rollback、失败/未接入 fixture、稳定错误码
tests/system-api.test.ts         /api/system/* 运行时校验（schema/字段类型/reasons/设备/restart 能力）、版本一致性、
                                 MOTU 识别、Content-Disposition 文件名、错误归类、改密校验、端点请求（CSRF/401）
tests/system-store.test.ts       info 控制器（加载/并发去重/失败保留旧数据/重试）+ 日志控制器（unit/行数切换、
                                 自动刷新 5s、隐藏暂停、失败保留旧日志、停止/重启）+ release 版本 store
tests/system-mock.test.ts        契约 mock 端到端：匿名 /healthz release 版本、401、三种 system fixture、
                                 日志参数校验与 503、诊断下载文件名/内容/无敏感字段、改密后旧会话失效、重启开关
tests/control-api.test.ts        M3 控制面：mute/surface/format/commit/abort 端点与运行时校验、逐 lane 增益模型
                                 （clamp/0.5dB/勾选补丁）、草稿面判定、错误码归类（含 409/501/503/网络）
tests/control-store.test.ts      M3 两阶段状态机：prepare/commit/abort/静音/设备重试、busy 互斥、token 失效清理、
                                 草稿保留/清空、历史 8 条、错误分类
tests/control-mock.test.ts       契约 mock 端到端：静音回读 (/api/status)、音量两阶段 + 单草稿槽 409 + abort、
                                 模式 changed/off+静音联动、格式 96k/64 + device_restart、unavailable fixture 503
tests/channels-api.test.ts       M4：khaen-channels/v1 校验、plan 上传/历史/预览解析（嵌套+扁平形态）、MOTU 标签、
                                 声道补丁校验、sha256、错误归类（含 host_link_unsupported/not_wired/invalid_plan）
tests/channels-store.test.ts     M4 控制器：方案上传（本地/服务端 sha 比对）、prepare/commit/abort、回滚草稿、
                                 声道 prepare/commit（501 保留草稿）、busy 互斥、加载去重
tests/channels-mock.test.ts      契约 mock 端到端：channels GET/PUT+commit 501、plan upload 幂等/sha 校验、
                                 history/inspect、rollback 草稿、degraded/unavailable fixture
```

当前共 **306** 个测试（Node 内置 test runner；基线 239 + M3 34 + M4 33），全部通过。

## 视觉验收（浏览器实测，evidence 在 `artifacts/browser/`）

- 视口 390x844 与 1440x900 × 深/浅主题 × zh-CN/en-US 共 8 组：`document.documentElement.scrollWidth <= innerWidth`
  （无横向溢出），卡片网格 390 单列 / 1440 三列；真实浏览器（Chromium/Playwright）实测 49 项断言全通过；
- M5 校准页/8 步向导/job 详情同矩阵浏览器实测 **133 项断言全通过**（`artifacts/qa/qa-calibration.mjs`，DEV-ONLY）：
  无横向溢出、主题/语言生效、无页面报错；success/empty/unavailable 三态诚实；第 2/5 步 NOT_AVAILABLE + 任务号；
  第 6 步 mock 引擎标「示意」；blocked 步骤「下一步」禁用且 title 带原因；**真实点击**应用 → 二次确认 → 回读 v4 /
  synthetic 回执 → 回滚 → 恢复 v3；
- 系统信息页/日志页同矩阵浏览器实测 **73 项断言全通过**（`artifacts/qa/qa-system.mjs`，DEV-ONLY）：
  无横向溢出、无页面报错；版本不一致警示条（crate v0.1.0 vs release v0.1.2）；顶栏/登录页页脚 release 版本；
  MOTU 标签；consistent 形态无警示、degraded 形态逐字段「未接入」+ 日志 503 原因 + 重启按钮隐藏；
  日志 unit/行数切换、复制、自动刷新；诊断 JSON 下载（文件名/内容/无敏感字段）；
  重启 rt-host 输入确认词二次确认（错误词拒绝）；改密 -> 旧会话失效 -> 新密码重新登录；
- 截图：`overview_zh_dark_1440x900.png`、`overview_zh_dark_390x844.png`、
  `overview_en_light_1440x900.png`、`overview_en_light_390x844.png`、
  `overview_zh_dark_stale_1440x900.png`（stale fixture）、`overview_zh_dark_missing_1440x900.png`
  （missing fixture，逐字段「未接入」）；
- 校准截图：`calibration_list_zh_dark_1440x900.png`、`calibration_list_zh_dark_390x844.png`、
  `calibration_list_en_light_1440x900.png`、`calibration_list_unavailable_zh_dark_1440x900.png`、
  `calibration_wizard_step{1,2,3,4,5,6,7,8}_zh_dark_1440x900.png`、`calibration_wizard_step3_zh_dark_390x844.png`、
  `calibration_wizard_step8_zh_dark_390x844.png`、`calibration_wizard_step8_en_light_1440x900.png`、
  `calibration_wizard_step3_blocked_zh_dark_1440x900.png`、`calibration_wizard_step8_applied_zh_dark_1440x900.png`、
  `calibration_job_zh_dark_{1440x900,390x844}.png`；
- M3/M4 同矩阵浏览器实测 **88 项断言全通过**（`artifacts/qa/qa-m3m4.mjs`，DEV-ONLY）：
  无横向溢出、主题/语言生效、无页面报错；音量页静音回读（/api/status）与取消静音、逐 lane 草稿 22 行 + 0.5 dB 步进 +
  prepare→commit→desktop readback、主音量/当前增益 NOT_AVAILABLE 卡片、模式 analog 禁用；格式页 96k/64 两阶段 +
  device_restart_required 提示 + 重试回读；声道页 22ch MOTU 映射/已知标签/未知 OUT n、plan 上传 sha 匹配 + 历史回滚二次确认、
  热重配 prepare 预览 + commit 501 诚实显示、degraded/unavailable 逐字段「未接入」+ 任务号；
- M3/M4 截图：`m3m4_volume_zh-CN_dark_1440x900.png`、`m3m4_volume_zh-CN_dark_390x844.png`、
  `m3m4_volume_en-US_light_1440x900.png`、`m3m4_volume_committed_zh_dark_1440x900.png`、
  `m3m4_volume_unavailable_zh_dark_1440x900.png`、`m3m4_format_zh-CN_dark_1440x900.png`、
  `m3m4_format_zh-CN_dark_390x844.png`、`m3m4_format_en-US_light_1440x900.png`、
  `m3m4_format_committed_zh_dark_1440x900.png`、`m3m4_channels_zh-CN_dark_1440x900.png`、
  `m3m4_channels_zh-CN_dark_390x844.png`、`m3m4_channels_en-US_light_1440x900.png`、
  `m3m4_channels_committed_zh_dark_1440x900.png`、`m3m4_channels_degraded_zh_dark_1440x900.png`、
  `m3m4_channels_unavailable_zh_dark_1440x900.png`；
- 系统页截图：`system_{zh-CN,en-US}_{dark,light}_{1440x900,390x844}.png`、`system_consistent_zh_dark_1440x900.png`、
  `system_degraded_zh_dark_1440x900.png`、`system_logs_restart_zh_dark_1440x900.png`、
  `system_logs_en_light_390x844.png`（`artifacts/` 不入库）。

## 开发

```bash
cd webui
pnpm dev    # http://localhost:5173，/api 反代到 KHAEN_DEV_API（默认 http://127.0.0.1:8730）
pnpm build && KHAEN_DEV_API=http://127.0.0.1:8790 pnpm preview   # 预览 dist，/api 反代到 mock
```

开发/预览代理转发 `/api`、`/__mock`（仅契约 mock）与匿名 `/healthz`（release 版本来源，生产由 daemon 同源托管，无代理、无外网资源）。

## 主题

- 选择器：`html[data-theme='dark|light']` + `html.dark`（Element Plus 暗色变量）；
- token：`src/styles/tokens.css`（深色默认 + 浅色覆盖 + Element Plus 变量映射），组件只引用变量不写死颜色；
- 首帧：`public/theme-init.js`（外置同步脚本，满足 daemon CSP `script-src 'self'`）在挂载前写 DOM，避免闪白/闪黑；
- 运行时：`src/composables/useTheme.ts`（`system` 模式下监听 `prefers-color-scheme`）。

## i18n

- 源语言 `src/i18n/zh-CN.ts`；`en-US.ts` 用 `Messages` 类型（`typeof zhCN`）在 **typecheck 阶段**强制键集对等；
- 键命名 `<scope>.<page|component>.<key>`；后端稳定错误码在前端映射成本地化文案（`src/api/errors.ts`、
  `src/api/status.ts` 的 classifyStatusError）；
- 新增文案必须同时改两份语言文件；`node scripts/i18n-check.mjs` 会检查缺失/多余/重复/空值、
  源码里引用的键是否存在、以及 `.vue`/`.ts` 是否硬编码中文（注释除外）。

## 目录

```
webui/
├─ index.html                 入口（含外置 theme-init.js）
├─ public/{favicon.svg,theme-init.js}
├─ scripts/
│  ├─ i18n-check.mjs          i18n 门禁脚本
│  ├─ mock-auth-server.mjs    /api/auth/*、/api/status、/api/calibration/*、/api/system/* 与 M3/M4 控制面契约 mock
│  │                          （开发/测试，不进 dist）
│  ├─ status-fixtures.mjs     /api/status 三种 fixture（present/stale/missing）
│  ├─ calibration-fixtures.mjs 校准四种 fixture（empty/success/failed/unavailable）+ mock job 状态机
│  ├─ system-fixtures.mjs     系统信息三种 fixture（mismatch/consistent/degraded）+ 日志/诊断 mock 数据
│  ├─ control-fixtures.mjs    M3 控制面两种 fixture（wired/unavailable）
│  ├─ channels-fixtures.mjs   M4 方案/声道三种 fixture（wired/degraded/unavailable）
│  └─ mock-test-support.mjs   单测辅助（不进 dist）
├─ tests/*.test.ts            Node 内置 test runner 单测（不进 dist）
├─ src/
│  ├─ main.ts App.vue config.ts env.d.ts
│  ├─ api/{client.ts,auth.ts,errors.ts,status.ts,calibration.ts,system.ts,control.ts,channels.ts}
│  │                           REST 客户端 / 认证 / 错误码映射 / 状态聚合 / 校准 REST + gate 纯函数
│  │                           / 系统信息 REST + 版本一致性/设备识别/错误归类
│  │                           / M3 控制面（mute/surface/format/commit/abort/device retry + 增益模型）
│  │                           / M4 方案与声道（khaen-channels/v1 + plan upload/history/inspect + sha256）
│  ├─ auth/validation.ts      表单本地校验
│  ├─ components/             AppShell / PageHeader / PanelCard / StatusBadge / TruthTag / StatRow
│  │  ├─ calibration/         WizardStepper / GateCard / JobTimeline / VerdictBadge / UploadCard /
│  │  │                       JobResultPanel / ApplyPanel
│  │  ├─ control/             ControlErrorNotice（稳定码 + 本地化文案 + 重试）
│  │  └─ system/              SystemLogsPanel / PasswordChangeCard / RestartCard
│  ├─ composables/            useTheme / useViewport / useStatusPolling
│  ├─ i18n/                   index.ts + zh-CN.ts（源）+ en-US.ts
│  ├─ router/                 index.ts（路由表 + 守卫）+ guard.ts（纯决策：next 防开放重定向）
│  ├─ stores/                 session.ts（认证会话）+ status.ts（概览聚合状态/轮询控制器）
│  │                          + calibration.ts（向导配置/state/上传/job 轮询/apply 回滚）
│  │                          + system.ts（系统信息/日志控制器）+ version.ts（release 版本）
│  │                          + control.ts（M3 单草稿槽两阶段 + 静音/设备重试 + 操作历史）
│  │                          + channels.ts（M4 方案上传/预览/历史/回滚 + 声道两阶段）
│  ├─ styles/                 tokens.css + main.css
│  ├─ types/                  ui.ts + auth.ts
│  ├─ utils/                  format.ts（Intl/单位/省略）+ wav.ts（WAV 头解析/格式校验）
│  └─ views/                  LoginView / OverviewView / CalibrationView /
│                             CalibrationWizardView / CalibrationJobView /
│                             SystemView / SystemLogsView / VolumeView / FormatView /
│                             ChannelsView
├─ dist/                      **构建产物，提交入库**
└─ vite.config.ts tsconfig.json package.json pnpm-lock.yaml
```

## 验收命令（本轮）

```bash
pnpm test && pnpm typecheck && node scripts/i18n-check.mjs && pnpm build
# 连续两次 pnpm build 的 dist 逐文件 SHA256 必须一致（确定性构建）；dist 不得包含 scripts/ 的 mock/fixture 字符串
# 浏览器视觉/交互验收（先起 mock + preview）：
#   node scripts/mock-auth-server.mjs --port 8790 --calibration-fixture success
#   KHAEN_DEV_API=http://127.0.0.1:8790 pnpm preview
#   node artifacts/qa/qa-calibration.mjs   # DEV-ONLY；截图写 artifacts/browser/（不入库）
#   node artifacts/qa/qa-system.mjs        # DEV-ONLY；系统信息页/日志页 73 项浏览器断言
#   node scripts/mock-auth-server.mjs --port 8790 --control-fixture wired --channels-fixture wired
#   node artifacts/qa/qa-m3m4.mjs          # DEV-ONLY；M3/M4 88 项浏览器断言（音量/格式/声道）
```
