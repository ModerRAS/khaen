# khaen webui

khaen DSP 引擎的 Web 控制台：Vue 3 + TypeScript + Element Plus + Vite + vue-router + vue-i18n。
由 khaen-daemon 同源托管（编译期 `include_bytes!` 内嵌 `webui/dist`），运行期不需要 Node / Python / 反代。

> 规划：`HANDOFF/20261126-khaen-webui-plan/PLAN.md`（仓库外）§11 UI 布局、§12 主题与 i18n、§10 Dirac 向导。
> 当前里程碑：**M0/M1 骨架**（脚手架 + 双主题 + i18n + 登录/概览静态壳）。

## 当前状态（M0/M1 骨架）

已落地：

- 工程骨架：Vite 6 + Vue 3.5 + TS 5.9 + Element Plus 2.14 + vue-router 4 + vue-i18n 11（pin 版本，见 `package.json`）；
- 深/浅双主题：PLAN §12.1 的 token 全量落地（`src/styles/tokens.css`），首帧由 `public/theme-init.js` 防闪，
  三态（跟随系统/深色/浅色）持久化到 `localStorage['khaen.theme']`，顶栏一键切换；
- i18n：`zh-CN`（源语言，完整）+ `en-US`（键集对等），全部界面文案走 `t()`，
  `scripts/i18n-check.mjs` 校验键集对等 / 重复键 / 空值 / 源码引用 / 硬编码中文；
- 路由：`/login`（初始化 / 登录两态**静态壳**，本地校验，提交只提示未接入）、
  `/`（概览占位壳，`AppShell` 侧栏 + 顶栏 + 状态带 + 8 张卡，全部「未接入」）；
- 路由守卫骨架（`ensureSession()`，M1 接真实 API 后自动生效）。

**明确未接入（不展示假数据）**：

- 认证 API（`/api/auth/status|session|setup|login|logout`）→ M1；登录按钮只做本地校验并提示「M1 未接入」；
- 聚合状态 `GET /api/status` → M2；概览卡所有数值显示「—」+「未接入」标签；
- 音量/方案与声道/路由/格式/校准时/系统/日志页面 → M3–M6，菜单灰显并标注里程碑；
- 校准向导（PLAN §10）→ M5，本轮**只做骨架，不做向导**。

## 构建与 dist 入库

`webui/dist/` 是**提交进仓库**的构建产物：daemon 在编译期把它整个编进二进制，
所以没有 Node 工具链的人也能直接 `cargo build` 出带控制台的 daemon。

```bash
cd webui
pnpm install --frozen-lockfile   # 首次或依赖变更（提交 pnpm-lock.yaml）
pnpm build                       # 产物写入 webui/dist/
git add dist && git commit       # dist 与源码同一次提交
```

纪律：**改完 `src/`、`public/`、`index.html` 或 `vite.config.ts`，必须重新 `pnpm build` 并一起提交 `dist/`。**
只改源码不重建，daemon 发出来的仍是旧界面；只提交 dist 不改源码，下次构建会覆盖。

## 门禁（提交前必须全绿）

```bash
cd webui
pnpm build
pnpm typecheck                 # vue-tsc --noEmit
node scripts/i18n-check.mjs    # 键集对等 + 无硬编码中文
```

## 开发

```bash
cd webui
pnpm dev    # http://localhost:5173，/api 反代到 KHAEN_DEV_API（默认 http://127.0.0.1:8730）
```

开发代理只转发 `/api`（生产由 daemon 同源托管，无代理、无外网资源）。

## 主题

- 选择器：`html[data-theme='dark|light']` + `html.dark`（Element Plus 暗色变量）；
- token：`src/styles/tokens.css`（深色默认 + 浅色覆盖 + Element Plus 变量映射），组件只引用变量不写死颜色；
- 首帧：`public/theme-init.js`（外置同步脚本，满足 daemon CSP `script-src 'self'`）在挂载前写 DOM，避免闪白/闪黑；
- 运行时：`src/composables/useTheme.ts`（`system` 模式下监听 `prefers-color-scheme`）。

## i18n

- 源语言 `src/i18n/zh-CN.ts`；`en-US.ts` 用 `Messages` 类型（`typeof zhCN`）在 **typecheck 阶段**强制键集对等；
- 键命名 `<scope>.<page|component>.<key>`；后端稳定错误码在前端映射成本地化文案；
- 新增文案必须同时改两份语言文件；`node scripts/i18n-check.mjs` 会检查缺失/多余/重复/空值、
  源码里引用的键是否存在、以及 `.vue`/`.ts` 是否硬编码中文（注释除外）。

## 目录

```
webui/
├─ index.html                 入口（含外置 theme-init.js）
├─ public/{favicon.svg,theme-init.js}
├─ scripts/i18n-check.mjs      i18n 门禁脚本
├─ src/
│  ├─ main.ts App.vue config.ts env.d.ts
│  ├─ api/                     （M1/M2 起：REST 客户端与类型，本轮为空）
│  ├─ components/              AppShell / PageHeader / PanelCard / StatusBadge / TruthTag
│  ├─ composables/             useTheme / useViewport
│  ├─ i18n/                    index.ts + zh-CN.ts（源）+ en-US.ts
│  ├─ router/                  路由表 + 守卫骨架
│  ├─ stores/                  session（骨架）
│  ├─ styles/                  tokens.css + main.css
│  ├─ types/                   UI 共享类型
│  └─ views/                   LoginView / OverviewView
├─ dist/                       **构建产物，提交入库**
└─ vite.config.ts tsconfig.json package.json pnpm-lock.yaml
```

## 验收命令（本轮）

```bash
pnpm build && pnpm typecheck && node scripts/i18n-check.mjs
```
