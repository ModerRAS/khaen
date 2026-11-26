# khaen webui

khaen DSP 引擎的 Web 控制台：Vue 3 + TypeScript + Element Plus + Vite + vue-router + vue-i18n。
由 khaen-daemon 同源托管（编译期 `include_bytes!` 内嵌 `webui/dist`），运行期不需要 Node / Python / 反代。

> 规划：`HANDOFF/20261126-khaen-webui-plan/PLAN.md`（仓库外）§4 API、§5 路由、§6.1 登录、§6.2 概览、§11 UI 布局、§12 主题与 i18n。
> 当前里程碑：**M2（概览页接真实聚合状态）**。后端 `GET /api/status`（T35a）由并行任务实现，前端按 PLAN §4.4 契约
> + 契约 mock + 单测完成；与真实 Rust 后端的联调由 lead 在合并后执行（本任务 NOT_RUN）。

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

### 明确未接入（不展示假数据）

- 音量/方案与声道/路由/格式/校准/系统/日志页面 -> M3–M6，菜单灰显并标注里程碑；
- 校准向导（PLAN §10）-> M5，概览页只读展示最近 job（无操作入口）；
- 真实 Rust 后端联调（T35a 并行实现，未合并）-> 由 lead 合并后执行；本任务用契约 mock + 单测验证前端行为。

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

mock 只实现 PLAN §4.1/§4.2 与 §4.4 的前端契约，用于前端自测，**不能替代真实 Rust/axum 后端**。

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
```

## 视觉验收（浏览器实测，evidence 在 `artifacts/browser/`）

- 视口 390x844 与 1440x900 × 深/浅主题 × zh-CN/en-US 共 8 组：`document.documentElement.scrollWidth <= innerWidth`
  （无横向溢出），卡片网格 390 单列 / 1440 三列；真实浏览器（Chromium/Playwright）实测 49 项断言全通过；
- 截图：`overview_zh_dark_1440x900.png`、`overview_zh_dark_390x844.png`、
  `overview_en_light_1440x900.png`、`overview_en_light_390x844.png`、
  `overview_zh_dark_stale_1440x900.png`（stale fixture）、`overview_zh_dark_missing_1440x900.png`
  （missing fixture，逐字段「未接入」）（`artifacts/` 不入库）。

## 开发

```bash
cd webui
pnpm dev    # http://localhost:5173，/api 反代到 KHAEN_DEV_API（默认 http://127.0.0.1:8730）
pnpm build && KHAEN_DEV_API=http://127.0.0.1:8790 pnpm preview   # 预览 dist，/api 反代到 mock
```

开发/预览代理只转发 `/api`（生产由 daemon 同源托管，无代理、无外网资源）。

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
│  ├─ mock-auth-server.mjs    /api/auth/* + /api/status 契约 mock（开发/测试，不进 dist）
│  ├─ status-fixtures.mjs     /api/status 三种 fixture（present/stale/missing）
│  └─ mock-test-support.mjs   单测辅助（不进 dist）
├─ tests/*.test.ts            Node 内置 test runner 单测（不进 dist）
├─ src/
│  ├─ main.ts App.vue config.ts env.d.ts
│  ├─ api/{client.ts,auth.ts,errors.ts,status.ts}  REST 客户端 / 认证 / 错误码映射 / 状态聚合
│  ├─ auth/validation.ts      表单本地校验
│  ├─ components/             AppShell / PageHeader / PanelCard / StatusBadge / TruthTag / StatRow
│  ├─ composables/            useTheme / useViewport / useStatusPolling
│  ├─ i18n/                   index.ts + zh-CN.ts（源）+ en-US.ts
│  ├─ router/                 index.ts（路由表 + 守卫）+ guard.ts（纯决策：next 防开放重定向）
│  ├─ stores/                 session.ts（认证会话）+ status.ts（概览聚合状态/轮询控制器）
│  ├─ styles/                 tokens.css + main.css
│  ├─ types/                  ui.ts + auth.ts
│  ├─ utils/                  format.ts（Intl/单位/省略）
│  └─ views/                  LoginView / OverviewView
├─ dist/                      **构建产物，提交入库**
└─ vite.config.ts tsconfig.json package.json pnpm-lock.yaml
```

## 验收命令（本轮）

```bash
pnpm test && pnpm typecheck && node scripts/i18n-check.mjs && pnpm build
```
