# khaen webui

khaen DSP 引擎的 Web 控制台：Vue 3 + TypeScript + Element Plus + Vite + vue-router + vue-i18n。
由 khaen-daemon 同源托管（编译期 \`include_bytes!\` 内嵌 \`webui/dist\`），运行期不需要 Node / Python / 反代。

> 规划：\`HANDOFF/20261126-khaen-webui-plan/PLAN.md\`（仓库外）§4 API、§5 路由、§6.1 登录、§11 UI 布局、§12 主题与 i18n。
> 当前里程碑：**M1（认证接线）**。后端认证（T34）由另一个任务并行实现；前端已按 PLAN §4.2 契约接好，并用契约 mock + 单测覆盖，真实联调在合并后执行。

## 当前状态（M1：认证 / 会话 / 路由守卫）

已落地：

- 工程骨架：Vite 6 + Vue 3.5 + TS 5.9 + Element Plus 2.14 + vue-router 4 + vue-i18n 11（pin 版本，见 \`package.json\`）；
- 深/浅双主题：PLAN §12.1 的 token 全量落地（\`src/styles/tokens.css\`），首帧由 \`public/theme-init.js\` 防闪，
  三态（跟随系统/深色/浅色）持久化到 \`localStorage['khaen.theme']\`，顶栏一键切换；
- i18n：\`zh-CN\`（源语言，完整）+ \`en-US\`（键集对等），全部界面文案走 \`t()\`，
  \`scripts/i18n-check.mjs\` 校验键集对等 / 重复键 / 空值 / 源码引用 / 硬编码中文；
- **认证 API 客户端**（\`src/api/client.ts\` + \`src/api/auth.ts\`）：
  - 所有请求 \`credentials: 'include'\`（Cookie HttpOnly，前端不读 Cookie）；
  - 写方法在拿到会话 csrf 后自动带 \`X-KHAEN-CSRF\`；登录/初始化等匿名接口显式不带；
  - 统一解析 \`{"error":{"code","message","details"}}\`，网络层失败归为 status=0；
  - 非探测请求 401 统一回调（清态 + 跳登录）；\`/api/auth/session\` 等探测请求不触发自跳；
  - 可选校验响应头 \`X-KHAEN-API-Version\`（不匹配只告警一次，不阻断）；
- **会话 store**（\`src/stores/session.ts\`）：\`configured / authenticated / csrfToken / expiresAt / checked / busy / lastError\`；
  \`ensureSession()\` 先 \`GET /api/auth/status\`，已登录再 \`GET /api/auth/session\` 取 csrf/expires_at（并发去重 + 缓存 + 过期自动重查）；
  登录/初始化成功保存 csrf；登出即使请求失败也清本地态；任意 401 清态并跳 \`/login?next=...&expired=1\`（只通知一次）；
- **路由守卫**（\`src/router/guard.ts\` + \`src/router/index.ts\`）：
  未登录访问受保护页 -> \`/login?next=<站内相对路径>\`；已登录访问 \`/login\` -> 回跳 next；
  \`next\` 只接受站内相对路径（拒绝绝对 URL、\`//host\`、反斜杠变体、控制字符、超长值），且不能回跳 \`/login\` 自身；
  后端不可达（会话状态 unknown）按未登录处理（fail closed），登录页展示"不可达 + 重试"，不冒充已登录；
- **登录 / 初始化页**（\`src/views/LoginView.vue\`）：由 \`/api/auth/status\` 决定 setup（密码 + 确认，>= 8 位且一致）
  或 signin（仅密码）；回车提交、提交中 loading/禁用、\`role=alert\` + \`aria-live\` 错误区；
  401 密码错误 / 429 限速 / 409 已初始化（自动切到登录态并提示）/ 后端不可达（重试）均映射为本地化文案；
  底部固定版本号 + 构建标识（\`KHAEN_WEBUI_BUILD_ID\`，无时间戳）；登录页与顶栏均可切换语言；
- **AppShell**：用户菜单"退出登录"调 \`POST /api/auth/logout\` 后回 \`/login\`；
- 路由：\`/login\`（公开）、\`/\`（概览占位壳，需要会话）；其余页面菜单灰显并标注计划里程碑。

## 明确未接入（不展示假数据）

- 聚合状态 \`GET /api/status\` → M2；概览卡所有数值显示「—」+「未接入」标签；
- 音量/方案与声道/路由/格式/校准/系统/日志页面 → M3–M6，菜单灰显并标注里程碑；
- 校准向导（PLAN §10）→ M5，本轮只做骨架，不做向导；
- 后端认证（T34）由并行任务实现，**真实联调 NOT_RUN**；本轮用契约 mock + 单测验证前端行为（见下）。

## 认证契约（PLAN §4.2）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | \`/api/auth/status\` | 匿名；\`{configured, authenticated}\` |
| POST | \`/api/auth/setup\` | \`{password, confirm}\` -> 201 \`{csrf, expires_in}\` + Set-Cookie；已配置 409 |
| POST | \`/api/auth/login\` | \`{password}\` -> 200 \`{csrf, expires_in}\` + Set-Cookie；失败 401，5 次/60s 后 429 |
| GET | \`/api/auth/session\` | \`{authenticated, csrf, expires_at}\`；未登录 401 |
| POST | \`/api/auth/logout\` | 清会话（幂等） |
| POST | \`/api/auth/password\` | \`{old, new}\`（登录 + CSRF） |

Cookie：\`HttpOnly; SameSite=Strict; Path=/\`（前端不读）；写请求带 \`X-KHAEN-CSRF: <csrf>\`；
错误体固定 \`{"error":{"code","message","details"}}\`（前端把稳定 code 映射成本地化文案，后端 message 只作详情）。

### 契约 mock（仅开发/测试，不进 dist）

\`\`\`bash
# 终端 1：启动 mock（内存态；重启即回到未初始化，可反复演练 setup）
node scripts/mock-auth-server.mjs --port 8790

# 终端 2：vite dev，/api 指向 mock
KHAEN_DEV_API=http://127.0.0.1:8790 pnpm dev
# Windows PowerShell: $env:KHAEN_DEV_API='http://127.0.0.1:8790'; pnpm dev
\`\`\`

mock 只实现 PLAN §4.1/§4.2 的 \`/api/auth/*\`（外加 \`/healthz\` 与 \`/api/status\` 的 401/501 占位），
用于前端自测，不能替代真实 Rust/axum 后端。

## 构建与 dist 入库

\`webui/dist/\` 是**提交进仓库**的构建产物：daemon 在编译期把它整个编进二进制，
所以没有 Node 工具链的人也能直接 \`cargo build\` 出带控制台的 daemon。

\`\`\`bash
cd webui
pnpm install --offline --frozen-lockfile   # 首次或依赖变更（提交 pnpm-lock.yaml）
pnpm build                                 # 产物写入 webui/dist/
git add dist && git commit                 # dist 与源码同一次提交
\`\`\`

纪律：**改完 \`src/\`、\`public/\`、\`index.html\` 或 \`vite.config.ts\`，必须重新 \`pnpm build\` 并一起提交 \`dist/\`。**
只改源码不重建，daemon 发出来的仍是旧界面；只提交 dist 不改源码，下次构建会覆盖。

构建必须**确定性**：构建信息只来自 \`KHAEN_WEBUI_BUILD_ID\`（缺省 \`local\`），不使用任何时间戳；
连续两次 \`pnpm build\` 的 dist 文件哈希必须一致（本轮已校验）。

## 门禁（提交前必须全绿）

\`\`\`bash
cd webui
pnpm test                      # Node 24 内置 test runner（.ts 直跑，无需额外依赖）
pnpm typecheck                 # vue-tsc --noEmit
node scripts/i18n-check.mjs    # 键集对等 + 无硬编码中文
pnpm build                     # 产物写 dist/
\`\`\`

## 单测 / mock 覆盖（\`tests/\`）

\`\`\`
tests/client.test.ts          fetch 封装：credentials、CSRF、错误体、401 回调、API 版本、超时/网络错误
tests/auth-api.test.ts        /api/auth/* 端点方法/请求体/CSRF 开关/响应校验
tests/session.test.ts         会话 store：ensureSession 两段探测、缓存/并发去重、登录/初始化/登出、401 清态跳转、过期
tests/guard.test.ts           路由守卫：next 防开放重定向、登录回跳、fail closed
tests/validation.test.ts      表单本地校验：必填/最短/两次一致/边界
tests/errors.test.ts          后端错误码 -> 归一化类别 + 本地化文案键
tests/mock-integration.test.ts 契约 mock 端到端：setup→session→改密→登出→登录、409、401+429、CSRF 403
\`\`\`

## 开发

\`\`\`bash
cd webui
pnpm dev    # http://localhost:5173，/api 反代到 KHAEN_DEV_API（默认 http://127.0.0.1:8730）
\`\`\`

开发代理只转发 \`/api\`（生产由 daemon 同源托管，无代理、无外网资源）。

## 主题

- 选择器：\`html[data-theme='dark|light']\` + \`html.dark\`（Element Plus 暗色变量）；
- token：\`src/styles/tokens.css\`（深色默认 + 浅色覆盖 + Element Plus 变量映射），组件只引用变量不写死颜色；
- 首帧：\`public/theme-init.js\`（外置同步脚本，满足 daemon CSP \`script-src 'self'\`）在挂载前写 DOM，避免闪白/闪黑；
- 运行时：\`src/composables/useTheme.ts\`（\`system\` 模式下监听 \`prefers-color-scheme\`）。

## i18n

- 源语言 \`src/i18n/zh-CN.ts\`；\`en-US.ts\` 用 \`Messages\` 类型（\`typeof zhCN\`）在 **typecheck 阶段**强制键集对等；
- 键命名 \`<scope>.<page|component>.<key>\`；后端稳定错误码在前端映射成本地化文案（\`src/api/errors.ts\`）；
- 新增文案必须同时改两份语言文件；\`node scripts/i18n-check.mjs\` 会检查缺失/多余/重复/空值、
  源码里引用的键是否存在、以及 \`.vue\`/\`.ts\` 是否硬编码中文（注释除外）。

## 目录

\`\`\`
webui/
├─ index.html                 入口（含外置 theme-init.js）
├─ public/{favicon.svg,theme-init.js}
├─ scripts/
│  ├─ i18n-check.mjs          i18n 门禁脚本
│  ├─ mock-auth-server.mjs    /api/auth/* 契约 mock（开发/测试，不进 dist）
│  └─ mock-test-support.mjs   单测辅助（不进 dist）
├─ tests/*.test.ts            Node 内置 test runner 单测（不进 dist）
├─ src/
│  ├─ main.ts App.vue config.ts env.d.ts
│  ├─ api/{client.ts,auth.ts,errors.ts}   REST 客户端 / 认证端点 / 错误码映射
│  ├─ auth/validation.ts      表单本地校验
│  ├─ components/             AppShell / PageHeader / PanelCard / StatusBadge / TruthTag
│  ├─ composables/            useTheme / useViewport
│  ├─ i18n/                   index.ts + zh-CN.ts（源）+ en-US.ts
│  ├─ router/                 index.ts（路由表 + 守卫）+ guard.ts（纯决策：next 防开放重定向）
│  ├─ stores/                 session.ts（configured/authenticated/csrf/expiresAt）
│  ├─ styles/                 tokens.css + main.css
│  ├─ types/                  ui.ts + auth.ts
│  └─ views/                  LoginView / OverviewView
├─ dist/                      **构建产物，提交入库**
└─ vite.config.ts tsconfig.json package.json pnpm-lock.yaml
\`\`\`

## 验收命令（本轮）

\`\`\`bash
pnpm test && pnpm typecheck && node scripts/i18n-check.mjs && pnpm build
\`\`\`
