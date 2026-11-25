# KHAEN 控制台（Vue 3 + TypeScript + Vite）

`khaen-rt-daemon` 在 80 端口直接托管的控制台前端。它取代了 T70 `web.rs` 内嵌的手写
vanilla 页面，以及与 daemon 并行的旧独立网页服务（原 `web/` crate `khaen-audio-web`，已删除）。

- 构建产物 `dist/` **提交进仓库**，daemon 在编译期用 `include_bytes!` 内嵌；
  没有 Node 工具链的人也能直接 `cargo build` 出带控制台的 daemon。
- 运行时不需要任何 Python/Node 进程或反代：daemon 自己发静态资源 + `/api/*` + `/ws`。

## 目录

```
index.html             Vite 入口（外置 theme-init.js 以满足 CSP script-src 'self'）
public/                favicon.svg、theme-init.js（原样拷到 dist 根）
src/
  api/client.ts        REST 客户端（同源 Cookie + X-KHAEN-CSRF；错误统一为 ApiError）
  api/types.ts         后端契约类型（依据 rt-stack/crates/control/src/rest.rs）
  stores/session.ts    会话（登录 / setup / token / 登出；CSRF 生命周期）
  stores/runtime.ts    运行时状态共享轮询（引用计数，5s，页面隐藏时暂停）
  stores/live.ts       WS（T24 兼容子集；不可用时显式降级为 REST 轮询）
  stores/content.ts    内容布局 / 中置下混契约（GET/PUT /api/content/layout）
  components/          外壳 / 真实性徽标 / 回执面板 / 内容布局面板 / 复制 / 页头
  views/               七个页面
  styles/main.css      深色主题、断点（<768 / 768–1199 / ≥1200）、安全区、触控目标
cef/                   （无）
dist/                  构建产物（**已提交**，见下）
scripts/               Playwright 证据采集脚本（evidence*.mjs / api-snapshot.mjs 等，非构建依赖）
```

## 构建产物 dist/ 是提交进仓库的

`dist/` **不是**被忽略的构建目录，而是 daemon 的编译输入：

```
webui/dist/                       rt-stack/crates/control/build.rs（编译期扫描 + 生成静态表）
├── index.html                    └─> include_bytes! 内嵌进 khaen-rt-daemon
├── favicon.svg
├── theme-init.js
└── assets/*.js|*.css（文件名带内容 hash）
```

因此：**改完 `src/`、`public/`、`index.html` 或 `vite.config.ts`，必须重新构建并把
`webui/dist/` 一起提交。** 只改源码不重建，daemon 发出来的仍是旧界面；只提交 dist 不改源码，
下次构建会把你的改动覆盖掉。

```bash
cd webui
pnpm install --prefer-offline     # 首次
pnpm build                        # 产物写入 webui/dist/
git add dist && git commit        # dist 与源码同一次提交
```

校验构建与源码一致（可选）：`pnpm build` 后 `git status --short webui/dist` 应为空。

## 技术栈

- Vue 3.5 + TypeScript 5.9 + Vite 6（`base: '/' `）
- Vue Router 4，history 模式；SPA 回退由 daemon 提供（见下）
- Element Plus 2.14（深色优先、可切浅色）+ `@element-plus/icons-vue`
- 按需引入：`unplugin-auto-import`（vue/vue-router）+ `unplugin-vue-components`（Element Plus）
- 全部资源本地打包，**无 CDN / 无外网字体**；中文界面
- 构建信息注入：`__APP_VERSION__` / `__APP_BUILD_TIME__` / `__APP_BUILD_ID__`
  （`KHAEN_WEBUI_BUILD_ID` 环境变量，缺省 `local`），系统信息页展示

## 页面（路由）

| 路由 | 页面 | 说明 |
| --- | --- | --- |
| `/login` | 登录 / 首次设置 | 密码 Cookie 会话；错误回显；token 登录折叠为高级选项；`setup_required` 时给出「后端未配置密码」警示 |
| `/` | 概览 | 引擎真实性徽标、运行状态、模式、采样率/块长、计数、音量/静音、当前 job、target/graph/coeff |
| `/volume` | 音量与静音 | 滑杆 + 数字输入 + 静音开关 + 回执区（synthetic / actual / unsupported 明确区分） |
| `/routing` | 路由矩阵 | role→DAC 编辑 + PUT target → prepare → commit；内容布局/中置下混面板 |
| `/format` | 音频格式 | 当前值 + 后端允许值 + UI 参考值（禁用）+ 应用回执 |
| `/calibration` | 校准 | job 列表/详情/状态机时间线、启动 full/retest/emperor、取消/apply/retest/rollback |
| `/system` | 系统信息 | 版本/构建/系数、工作模式切换、WS 实时状态、AES67/PTP/CueMix 等「待接线」占位卡 |

> 注意：daemon 侧 `/calibration`、`/calibration/style.css`、`/calibration/app.js` 仍是
> T70 的手写校准向导页（`crates/control/assets/calibration/`，生产在用的正式页面），
> 优先级高于本 SPA 的同名路由；其余非 `/api` 路径都由本 SPA 接管。

## daemon 侧行为（`rt-stack/crates/control`）

- `build.rs` 扫描 `webui/dist`（路径相对 `CARGO_MANIFEST_DIR`：`../../../webui/dist`），
  生成 `(path → bytes, content-type)` 静态表，逐文件 `include_bytes!` 编进二进制；
  `rerun-if-changed` 指向 dist 目录，重新构建 dist 后 `cargo build` 自动重编。
- 路由：`/api/*` 与 `/healthz` 走原有 REST；`/ws` 走 WebSocket；其余 GET
  命中静态表就返回文件，未命中就返回 `index.html`（SPA 回退）。
- 缓存：`/assets/*`（文件名带内容 hash）`public, max-age=31536000, immutable`；
  `index.html`、`favicon.svg`、`theme-init.js` 与 SPA 回退一律 `no-store`。
- 安全：静态资源不需要登录，`/api/*` 仍要求会话（与改造前一致）。SPA 资源使用
  `style-src 'self' 'unsafe-inline'`（Element Plus 需要内联 style）；脚本仍只允许同源，
  本包唯一脚本是外置的 `theme-init.js`，无内联 script / 无 `eval`。
- 版本头：响应带 `X-KHAEN-API-Version` / `X-KHAEN-Web-Version`（daemon 版本）。

## 开发（本地 HMR）

```bash
cd webui
pnpm dev     # http://localhost:5173，/api、/healthz、/ws 反代到板端 daemon（vite.config.ts server.proxy）
```

开发代理只读转发；不改 API 路径（`/api/...` 与 daemon 完全一致）。

## scripts/

Playwright 证据采集脚本，不是构建依赖，只在需要截图/契约快照时手动运行：

```bash
KHAEN_BASE_URL=http://192.168.63.222 KHAEN_PASSWORD=... node scripts/smoke.mjs
```

默认目标 `http://192.168.63.222`（板端 80 端口 daemon），产物默认写入 `webui/artifacts/`（git 忽略）。
