# TSPI P1 兼容矩阵（T05 / compat-auditor）

> 状态：**v0.1 草案（2026-11-16）**，供 Lead 评审与 T13/T24/T25/T42/T44/T57/T60/T70/T74 消费。
> 本轮只写规范：**未实现代码、未启动服务、未接板、未运行 REST/WS、未 git commit**；所有"验证"仅指对既有源码/文档的静态核对。
> **2026-11-17 更新（Cursor 审计 F-32）**：T24 已落地 P1 ADD 的显式 prepare/commit/abort 三命令（§4 W23–W25），T42 R49 与 T70 消费同一 prepare/commit 语义；库内 `cargo test` 证据在 `crates/{compat,control}/tests`，**真实 host/板端路径仍为 NOT_RUN**。矩阵其余行口径不变。
> **2026-11-17 更新（Cursor 审计 F-21，task-184）**：新增 §3.7「R 矩阵 ↔ `crates/control/src/rest.rs` 路由落地对照」——R01-R06/R10-R14/R47-R49 已落地（ROUTED/RETIRED_ROUTE），R50/R51 为横切语义（CROSS_CUTTING），**R07-R09/R15-R46 共 35 行改为 `ADAPT-DEFERRED`/`KEEP-DEFERRED`（P1 不注册路由，逐行理由/owner 见 §3.7）**；`cargo test -p khaen-control rest` 的 `matrix_routes` 用例自动核对矩阵↔路由（含负向用例）；证据 `rt-stack/evidence/cursor-fix-matrix/`。
> 写域：本文件 `projects/tspi-audio-dsp/rt-stack/spec/compat-matrix.md`。其余项目文件与目录只读。
> 权威链（自上而下，冲突按此裁决，DEC §0）：用户最新要求 → `DSP-HOST-DECISION.md`（下称 DEC）→ `TSPI-RT-STACK-WBS.md`（下称 WBS）→ 既有契约稳定字段语义（校准/通道图/模式状态）→ 现有实现（只读参考）。
> 证据标记：**【验】**= 已逐行读源码/文档确认（静态，不等于运行验证）；**【推】**= 由已读材料推导、需实现或 Lead 裁决；**【未测】**= 无运行证据（板端/服务/WS/HTTP 均未实跑）。
> 引用策略（task-38 起）：DEC/WBS/API.md 一律用「文件路径 + §章节 / WBS 任务 ID」引用，不再写死行号与 hash；代码/契约仍用 file:line。输入快照 hash 仅见 §13.1（快照时间 2026-11-17 00:15）。

---

## 0. 作用、边界与非目标

**作用**：把"新栈必须兼容什么、可以退役什么、未知项怎么拒收"冻结成可判矩阵，作为 T24（WS 兼容子集）、T13（校准适配）、T25（channel-map 扩展点）、T42（REST）、T44（mode）、T57（overlay 角色）、T60（校准 job）、T70（Web）、T74（真实播放/Web）的设计输入与验收清单。

**边界**：
1. **P1 不实现通用上混/下混/节目矩阵造声道/内容自动上混 daemon**；只保输入 N/目标 O 的可变 channel-map 接口（选择、置零、显式拒绝、未来扩展）。**唯一例外（用户 2026-11-17 裁决；C09，不是通用 downmix）**：中置缺席时，含 C 音轨的中置内容不得丢失——在 plan 之前的内容适配层把 C 以 −3 dB 预混进 FL/FR（有中置布局 passthrough）；该项**已实现并已接入 host**（host 运行时接线 task-203；task-210 把实现迁入 `crates/rt-host/src/content_map.rs` 由 host 自持、移除 rt-host→compat 依赖，compat 层原型保留为旧 S1/兼容层参考；见 C09），仍属缺音箱必需处理，不是通用/节目 downmix。见 DEC §1、DEC §6、WBS T05、§6 C09。
2. **CamillaDSP 只作开发期 oracle**；P1 验收时无 C 进程、相关 unit 禁用或移除、失败只回自研 last-good 或 mute。见 DEC §0、DEC §7、DEC §7。
3. **不把既有文件契约当可随意重写对象**：校准 v1/v1.1、channelmap v2、layout-presets v2 已冻结语义；新栈可新增版本，不得静默改旧文件语义。见 WBS T13、WBS §5。
4. **不把"能 curl 到"当兼容**：真实 Web 日常操作与 actual version ack 才算过门。见 DEC §6、WBS T70、WBS T74。

**非目标**：不设计 DSP 内核、不在本文件定 REST 路径实现细节、不改板端、不退役任何服务、不测量保护限幅实际效果。

---

## 1. 现状快照与消费者

### 1.1 P1 目标（引用，不在本文件重述）

- 自有 RT host + DSP engine + I/O/control，Toslink 2.0 FL/FR 直出、目标多声道按布局到 4.1.2，真实日常 REST/WS Web 入口可用：WBS §1、DEC §0。
- 校准由自研 daemon 端到端（扫频/UMIK/时间轴/求 5×5/编译/热更/应用后复测/独立 holdout）：DEC §2、WBS §1。
- 唯一最终门为 DEC §7 的 7 门：G-play/G-web/G-cal/G-real/G-RT-foundation/G-no-C + 后续 G-performance（正式性能移 P2，不属 P1 签收）；W12 的 load/CV 引用属后续 KPI 口径（DEC §4）。
- 默认 4.1.2 角色映射 role→DAC：FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7、ch6 空；低带 dense 0..4→DAC 0/1/2/3/7；**不硬编码快照、必须解析 overlay 并校验实际设备宽度**：WBS §3、DEC §1、layout-presets.yml:219-229。

### 1.2 现状组件（S1，只读参考）

| 组件 | 当前职责 | 关键入口 | 证据 |
| --- | --- | --- | --- |
| khaen-audio-web（Rust/axum 0.8） | 当前唯一 Web/REST；默认 0.0.0.0:80；session+CSRF；聚合状态 | `web/src/routes/mod.rs:27-129`（60 条 route 条目/67 个方法操作）；`web/Cargo.toml:10` | 【验】 |
| tspi-mode（bash） | dante/aes67/off 唯一切换入口；exit 0/2/3/4/5/7/8/9/10（注释另含 1/6） | `audio/overlay/usr/local/bin/tspi-mode:2-14,37-51,454-492` | 【验】 |
| tspi-channelmap（Python） | channelmap v2 校验/渲染/apply/rollback 唯一权威；layout-presets v2 | `audio/overlay/usr/local/bin/tspi-channelmap:6-7,36-61,78-85` | 【验】 |
| tspi-upmix（Python） | **S1** 输入内容检测 + 双路径上混；读 Camilla WS RMS、切 Aux fader | `audio/overlay/usr/local/bin/tspi-upmix:1666-1717`；`upmix/docs/INPUT-MODE.md:10-118` | 【验】 |
| CamillaDSP 4.1.3 | 当前 DSP；WS `ws://127.0.0.1:1234`；4.x 无 GetXruns | `web/src/camilla.rs:3-7,435-448`；`web/docs/API.md §6` | 【验】 |
| AES67 daemon / Inferno+statime / ptp4l / phc2sys | AoIP 端点与 PTP | `audio/overlay/etc/aes67-daemon/daemon.conf:1-37`；`audio/overlay/etc/statime/inferno-ptpv1.toml`；`audio/overlay/etc/ptp4l-master-aes67.conf` | 【验】 |
| MOTU CueMix 5 | MOTU 控制面（UDP 1280 信标 + WS 1280 datastore） | `tools/motu/PROTOCOL-REPORT.md:6,14`；`web/docs/API.md §9.1` | 【验】 |
| 新栈（P1 目标） | 自研 RT host/DSP/I/O/control + Web；无 C 进程 | 本 WBS T01-T74；本文件约束其兼容面 | 【推】 |

### 1.3 消费者依赖（兼容矩阵必须保护的真实使用面）

| 消费者 | 依赖的兼容面 | 证据 |
| --- | --- | --- |
| 现有 Web UI `web/assets/app.js` | REST：auth、overview、mode、audio/format、camilladsp、ptp、aes67/inferno、alsa、motu、channelmap、input-mode；字段：overview 的 mode/system/camilla/ptp/alsa/services/service_notes/power、mode 的 status 字段、components.ptp/aoip/camilla/audio、channelmap view、input-mode | `web/assets/app.js:289-332,418-527,574-705,781-875,926-1005,1051-1119,1178-1259,1279-1432,1501-2166` 【验】 |
| 现有 Web 服务端 | `tspi-mode status/切换 --json`、`tspi-channelmap validate/render/apply/rollback`、Camilla WS 1234、systemd/journald、CueMix WS 1280、/proc/asound、/sys | `web/src/mode.rs:1113-1238`；`web/src/channelmap.rs:1600-2220`；`web/src/camilla.rs:287-325`；`web/src/config.rs:89,147-155,469` 【验】 |
| S1 内容检测 daemon | Camilla WS `GetCaptureSignalRms`、`SetFaderVolume`、`/run/tspi/input-content.json`、`/etc/tspi-audio/input-mode.json` | `audio/overlay/usr/local/bin/tspi-upmix:1691-1717`；`audio/overlay/etc/systemd/system/tspi-upmix-autodetect.service:12（ExecStart；:4-5 Wants/After 见 C07）` 【验】 |
| 校准 producer/consumer | calibration YAML v1/v1.1（字段/范围/未知 type 拒收）、channelmap v2 合并规则 | `calibration-spec/CONTRACT.md:26-196,245-258`；`calibration-spec/tools/validate_calibration.py` 【验】 |
| 新栈任务 | 本文件 R/W/Y/A/P/X 条与检查表 K 条 | 本文件；WBS T05/T13/T24/T60/T42/T44/T25/T70/T74【推】 |

> **F-21 口径（2026-11-17，task-184）**：Phase 1 运行镜像不安装旧 `khaen-audio-web`（`web/src/routes/camilladsp.rs` 文件头标「退役栈镜像」，F-04 已删旧 web 的 Camilla 启停路由；T71 退役切换见 DEC §6），旧 UI 不在 P1 消费面；T70 新 UI 只消费本文件已落地行（R01-R14/R47-R49）。因此旧 Web 专属路径（overview/system/camilladsp/ptp/aoip/alsa/motu/channelmap 扩展等）在 P1 按 §3.7 标 `*-DEFERRED`，**不注册路由**（`/api/*` 未知路径按 X01 返回 JSON 404）；若后续裁决恢复迁移期旧 UI 兼容，再按 §3.7 的 owner 逐行落地并改回 KEEP/ADAPT。

---

## 2. 兼容原则与处置码

### 2.1 处置码（本文件唯一口径）

| 码 | 含义 | 硬要求 |
| --- | --- | --- |
| **KEEP** | 路径/语义保持，后端可换实现 | 外部可观察字段与状态码不变；不新增必填字段 |
| **ADAPT** | 路径保留，后端改由新栈（RT host/control/daemon）实现 | 顶层字段与错误码保持；"已应用"必须返回 actual version/ack；不得启动 Camilla 或旧 C 服务 |
| **RETIRE** | P1 不再提供 | 一般：HTTP 410 + `{"error":{"code":"retired","replacement":...}}`；**唯一例外 R47 input-mode：GET 200 not_supported 状态壳 / PUT 400 not_supported**（UI 轮询兼容，见 X15）；必须同变更更新 UI；**禁止**静默 404/空 200/假成功；处置码必须唯一（R16=ADAPT-DEFERRED、R50=KEEP、R51=KEEP） |
| **ADD** | P1 新增 | 必须沿用既有错误 envelope 与认证；必须有 actual version/ack 与幂等/冲突语义 |
| **KEEP-DEFERRED / ADAPT-DEFERRED** | P1 不落地（旧 Web 专属路径；目标语义保留，待迁移期裁决或后续任务落地） | **不得注册路由**（`/api/*` 按 X01 JSON 404，不得空壳 200/假语义冒充）；逐行理由/owner 见 §3.7；不得以 KEEP/ADAPT 名义冒充已支持；落地时改回原码并同步 §3.7/§2.4；`cargo test -p khaen-control rest` 的 `matrix_routes` 检查强制该口径 |

### 2.2 四条硬规则

1. **不静默降级**：未知字段/未知 type/未知 schema 不得被丢弃后继续（channelmap 已按此实现 rc=2；校准未知 filter type 必须整份拒绝）。见 `tspi-channelmap:409-411`、`CONTRACT.md:137,251`。
2. **不假 ack**：任何"应用/切换/音量/校准 apply"响应必须在真正生效后返回，并带实际版本（graph/coeff/config hash）；prepare 与 commit 分离。见 DEC §5/§6；WBS T24。
3. **不双计/不双应用**：校准 curve 与渲染 P 不重复乘；极性只信校准字段（CONTRACT:123-124）；同一份 manifest 不得未校验就切 profile（DEC §1）。
4. **不回旧 C**：任何失败路径只能回自研 last-good 或 mute；不得 systemctl start camilladsp 或依赖 C 兜底（DEC §0/§7；WBS §8）。

### 2.3 兼容面总览（计数基线，供检查表核对）

- REST：router 60 条 route 条目、67 个方法操作（含 4 个公开端点）；本矩阵 R01-R51 逐组覆盖（routing 清单位置 `web/src/routes/mod.rs:29-123`，公开 120-123、fallback 125）。
- REST 落地状态（F-21，task-184）：已落地/横切 16 行 = ROUTED 13（R01-R06、R10-R14、R48、R49）+ RETIRED_ROUTE 1（R47）+ CROSS_CUTTING 2（R50、R51）；`ADAPT-DEFERRED`/`KEEP-DEFERRED` 35 行（R07-R09、R15-R46）不注册路由。唯一机器可读对照见 §3.7，由 `matrix_routes` 测试自动核对。
- WS：当前 web 客户端固定 12 条 live 读命令 + `GetConfig`/`SetConfigJson`/`SetVolume`；S1 daemon 另有 `GetCaptureSignalRms`/`SetFaderVolume`；P1 兼容子集见 §4。
- YAML/JSON 契约：channelmap v2、layout-presets v2、calibration v1 与 v1.1、input-content v1/input-mode v1 四组；另 target 导入草案与 P1 新 IR/manifest（§5）。
- 模式：off/aes67/dante + P1 `toslink`/`target_multichannel`（WBS T44）；`tspi-mode` exit 0..10（11 个取值：0/1/2/3/4/5/6/7/8/9/10）；`--restore` 为独立入口（不计入 exit 集）。
- chunksize：**runtime 仅 64/128**（生产 128、最终独立窗 64；T02 core-v1 Q-11）；32/256/512/1024 只作 UI/参考展示，REST 对非 64/128 返回 400/unsupported（见 R13/R14/Q09）。
- 保护：P01-P14 共 14 行（格式/削波、计数/故障、limiter/max gain、增益裕量、延迟/极性、回退图、音量/mute、校准扫频安全、xrun/超时、启动/断流、切换安全、校准应用、验收证据、未决参数）。

### 2.4 端点字段/版本策略（X02/Q03 冻结）

> **处置码与字段策略解耦**：本表“策略类别”只决定未知字段/版本行为；处置码以 §2.1/§3.7 与各 R 行为准（**已落地**：KEEP={R01-R06,R50,R51}、ADAPT={R10-R14}、RETIRE={R47}、ADD={R48,R49}；**F-21 降级**（P1 不注册路由）：KEEP-DEFERRED={R08,R09,R29-R33}、ADAPT-DEFERRED={R07,R15-R28,R34-R46}）。R19/R28 属 ADAPT-DEFERRED，下面“迁移白名单”只在落地后适用，不改变其 DEFERRED 状态；R50 属 KEEP，**不在 ADD**。

| 策略类别 | 端点/范围（含处置码） | 未知字段策略 | 版本策略 | 迁移动作 |
| --- | --- | --- | --- | --- |
| ADD | R48、R49（新增 calibration job / runtime 控制；§2.1 ADD） | 严格：未知字段 400 `unknown_field`（给字段名/路径） | 请求头 `X-TSPI-API-Version` 可选、缺省 v1 | 无历史包袱 |
| ADAPT / ADAPT-DEFERRED | 已落地 ADAPT=R10-R14；ADAPT-DEFERRED=R07、R15-R28、R34-R46（不含 R47；§2.1/§3.7） | 落地后按各 R 行冻结 request struct，未知字段 400 `unknown_field`；DEFERRED 行当前不注册路由 | 同上 | 落地 owner 见 §3.7 |
| KEEP / KEEP-DEFERRED | 已落地 KEEP=R01-R06、R50、R51；KEEP-DEFERRED=R08、R09、R29-R33（§2.1/§3.7） | 已落地行迁移期允许现有客户端额外字段忽略（白名单：R02-R06 认证字段等）；DEFERRED 行不注册路由 | 同上 | 白名单变更须更新本表并在 T70 回归；DEFERRED 落地见 §3.7 |
| RETIRE | R47（input-mode；§2.1 RETIRE） | GET 200 `{"status":"not_supported"}` / PUT 400 `not_supported`（唯一例外，见 X15） | 同上 | UI 同变更隐藏/禁用 |
| 迁移白名单（字段级，不改变处置码） | R19 `{db}`（ADAPT-DEFERRED；落地后按 ADAPT）、R28 `{device}` + `channel` 文档字段（ADAPT-DEFERRED；落地后按 ADAPT） | 仅豁免所列字段；其它未知字段仍按 ADAPT 严格 400 | 同上 | 落地并按 T70 审计后收紧 |

> 说明：R19/R28 的字段级白名单不把端点降级为 KEEP；R16/R50/R51 的处置码固定为 ADAPT-DEFERRED/KEEP/KEEP（§2.1/§3.7）；K57 按本表判。

---

## 3. REST 兼容矩阵（R01–R51）

> 说明：路径均为相对 `/api` 之外的全路径；"来源"给出现状实现/文档行；"处置"见 §2.1；【验】=静态核对，不表示运行过。

### 3.1 认证与系统（R01–R09）

| ID | 方法/路径 | 现状与来源 | 处置 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- | --- |
| R01 | GET /healthz | 公开；`{status:"ok",version,mock,uptime_secs,now_unix}`；安全头 | KEEP | 匿名可达；字段名/类型不变；version=新栈构建版本；生产 mock=false；CSP/X-Frame-Options/nosniff/Referrer-Policy/no-store 保持（API.md §1、web/src/routes/mod.rs:132-151） | 验：API.md §2、web/src/system.rs:21-29、web/src/routes/mod.rs:120、web/tests/api.rs:168-175 |
| R02 | GET /api/auth/status | 公开；`password_configured/setup_required/token_fallback/mock/min_password_len/recommended_password_len` | KEEP | 匿名；字段语义不变；setup_required 与 auth 文件一致 | 验：API.md §2、web/src/routes/mod.rs:121、web/src/routes/auth.rs:27-35（password_configured:28、token_fallback:31） |
| R03 | POST /api/auth/setup | 公开仅未配置；密码≥8 两次一致；成功即登录；已配置 409 | KEEP | 409 conflict 保持一致；Argon2id 写 0600；不得因新栈引入明文 | 验：API.md §2、web/tests/api.rs:280-332 |
| R04 | POST /api/auth/login | password 或 token；200+Set-Cookie+csrf_token；401；失败 5 次 429 | KEEP | 会话 TTL/锁定语义保持；header token 不需要 CSRF | 验：API.md §2、web/tests/api.rs:81-86,404-410 |
| R05 | GET /api/auth/session | 受保护；`authenticated/auth_kind/csrf_token/expires_in`；未认证 401 | KEEP | header token 时 csrf_token=null；401 error envelope 保持 | 验：API.md §2、web/src/routes/mod.rs:29、web/tests/api.rs:204-213 |
| R06 | POST /api/auth/logout | 受保护；`{ok:true}` | KEEP | 会话失效；幂等 | 验：API.md §2、web/tests/api.rs:231-237 |
| R07 | GET /api/overview | 聚合：`mock/version/uptime_secs/now_unix/system/mode/services/service_notes/camilla/ptp/aoip/alsa/power`；UI 依赖其字段 | ADAPT-DEFERRED | 上述顶层键保持；`camilla` 键改为新栈 runtime 摘要（同键名字段语义：live.state/version/volume/capture_rate/buffer_level/clipped_samples/config_path/xrun 或 UI 同步改）；`mode` 键保持 ModeSummary 字段；`power.allow_reboot/allow_shutdown` 保持 | 验：API.md §3、web/src/routes/overview.rs:17-64,119-179、web/assets/app.js:418-527；推断：字段语义替换需 UI 同步 |
| R08 | GET /api/system | `hostname/kernel/os/uptime_secs/cpu/memory/realtime/network/warnings` | KEEP-DEFERRED | 只读；不得暴露 token/密钥；温度/cpu 字段保持 | 验：API.md §3、web/assets/app.js:448-472、web/src/routes/system.rs |
| R09 | POST /api/system/reboot、/shutdown | body `{confirm:"REBOOT"/"SHUTDOWN"}`；配置显式开启否则 403 | KEEP-DEFERRED | 危险操作双确认保持；未开启 403 forbidden；不得因新栈默认放开 | 验：API.md §3、web/assets/app.js:1415,1432 |

### 3.2 模式与音频格式（R10–R14）

| ID | 方法/路径 | 现状与来源 | 处置 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- | --- |
| R10 | GET /api/mode | 当前模式+切换状态+steps+components+profiles+persisted+mock；401 | ADAPT | 顶层字段（mode/mode_label/target/state/step/progress/steps/last_error/warnings/enabled/restore_on_start/switch_script/strategy/profiles/script_status/persisted）保持给 UI；**必须接受 P1 值 off/aes67/dante/toslink/target_multichannel 并回 actual profile/layout/format**；实际 owner 换新栈 T44 | 验：API.md §4、web/src/routes/mode.rs:29-33、web/src/mode.rs:217-240、web/assets/app.js:574-705；WBS T44；推断：owner 换新栈 |
| R11 | PUT /api/mode | body `{mode}`；202 接受；在途 409；未启用 403；未知值 400 | ADAPT | 202/409/403/400 语义保持；**接受 toslink/target_multichannel 新值（WBS T44）**；切换由新栈唯一 owner；不得并行第二个模式切换；成功后 actual mode/profile 与 ack 一致 | 验：API.md §4、web/src/routes/mode.rs:69-86、web/src/config.rs:222-230；WBS T44；推断：新 control 扩展 |
| R12 | GET /api/mode/script-status | 主动执行 `tspi-mode status --json`；脚本缺失返回 available:false+message | ADAPT | 若保留脚本则 schema `tspi-audio/v1` 字段不变；若新栈不再用该脚本，必须返回 available:false+原因（不得 500），并由 /api/mode 提供等价 status | 验：web/src/routes/mode.rs:50-67、web/src/mode.rs:157-189,1351-1379；推断：脚本退役路径 |
| R13 | GET /api/audio/format | 返回当前模式采样率/位深/声道/chunksize/target_level/允许范围/默认值/xrun 线索/告警 | ADAPT | **runtime 仅 64/128 chunksize**（生产 128、最终独立窗 64；T02 core-v1 Q-11）；32/256/512/1024 仅 UI/参考展示，REST 对非 64/128 返回 400/unsupported；采样率 48k/96k；actual 必须来自新 host 实际 hwparams | 验：API.md §5、web/src/audio.rs:10-15,109-141；推断：新 host actual |
| R14 | PUT /api/audio/format | serde_yaml 校验→备份→写文件→WS SetConfigJson→失败 systemd restart→再失败回滚；位深/声道只读 | ADAPT | body 字段 `sample_rate/chunksize/target_level` 至少一个；非法 400；**chunksize 仅接受 64/128，其它值 400/unsupported（T02 Q-11）**；应用需 prepare/commit actual ack；不得通过 Camilla 配置或 systemd 重启生效 | 验：API.md §5、web/src/routes/audio.rs:65-106；推断：新栈生效机制 |

### 3.3 CamillaDSP 面（R15–R19，P1 无 C 进程）

| ID | 方法/路径 | 现状与来源 | 处置 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- | --- |
| R15 | GET /api/camilladsp | `service+live+configs`；live 含 version/state/stop_reason/config_path/title/capture_rate/processing_load/buffer_level/rate_adjust/clipped_samples/volume/mute | ADAPT-DEFERRED | 路径可保留但后端必须是新栈 runtime 状态（不得启动 C）；字段名保持或 UI 同步替换；xrun 只能由新栈计数/日志提供（C 4.x 无 GetXruns） | 验：API.md §6、web/assets/app.js:474-490,939、web/src/camilla.rs:433-471 |
| R16 | POST /api/camilladsp/start、/stop、/restart | systemd 操作；未允许 403；mock 返回 ok | **ADAPT-DEFERRED** | 路径保留、后端改新栈 runtime start/stop/restart（graph/epoch 级），不得 systemctl camilladsp；成功 200 `{ok:true,actual_version,...}`；未启用 403 forbidden；不可用 503 service_unavailable；失败不得假成功；若 T70 最终弃用该路径，改为统一 RETIRE+410 retired 并在 §2.1/§13.5 记录（单一状态码） | 验：API.md §6、web/src/routes/mod.rs:42-44、web/tests/api.rs:421-459；推断：新栈映射 |
| R17 | GET /api/camilladsp/configs、/config | 配置列表/读取 YAML；不存在 404；>2MiB 拒绝 | ADAPT-DEFERRED | 新栈 graph/coeff 目录与 manifest 列表可采用同形响应；不得把 Camilla YAML 当运行时真值；只读预览需给 actual version/hash | 验：API.md §6、web/src/camilla.rs:512-556；推断：路径映射 |
| R18 | POST /api/camilladsp/configs/switch、/config/validate | 校验+备份+切换+WS 应用+失败回滚；无效 400 | ADAPT-DEFERRED | prepare→commit 实际 ack；坏图不生效并返回原因；保留备份/last-good 回滚证据；不得 C 重启兜底 | 验：API.md §6、web/src/camilla.rs:131-149,337-412、web/tests/api.rs:535-580；推断：新栈语义 |
| R19 | POST /api/camilladsp/volume | 请求体 **`{"db": f64}`**（`web/src/routes/camilladsp.rs:207-222`；UI `web/assets/app.js:801` 也发 `{db}`）；-150..+50；响应 `{volume_db}`（:212-214）；旧 API.md §6 的 `{volume_db}` 是过时文档，不得作为新契约 | ADAPT-DEFERRED | 新栈契约以真实实现 `{db}` 为准（**不是 `volume_db`**）；越界 400；返回 actual dB + `volume_db`；不得只改前端；该字段列入 §2.4 KEEP/ADAPT 白名单，禁止 X02 严格化误杀 | 验：web/src/routes/camilladsp.rs:207-222、web/assets/app.js:801、web/src/camilla.rs:590-608、web/tests/api.rs:515-526 |

### 3.4 PTP、AoIP、ALSA、MOTU（R20–R33）

| ID | 方法/路径 | 现状与来源 | 处置 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- | --- |
| R20 | GET /api/ptp | 状态 offset/GM/portState/role/source/locked+服务+模式说明；401 | ADAPT-DEFERRED | 新栈唯一 PTP owner 提供同义字段；B1 无公开接口时须给明确 unknown 而非假 locked；PTP 锁≠USB 同钟 | 验：API.md §7、web/tests/api.rs:588-592；推断：owner 换新栈 |
| R21 | GET/PUT /api/ptp/config | 解析 ptp4l.conf（masterOnly/priority1）；PUT 备份+重启+失败回滚 | ADAPT-DEFERRED | 仅对仍存在的 linuxptp 服务生效；新栈若内建 PTP owner，接口必须显式映射或 410；PUT 需 400 校验+回滚证据 | 验：API.md §7、web/assets/app.js:1032；推断：PTP 归属待 Q05 |
| R22 | POST /api/ptp/start、/stop、/restart | 操作 ptp4l/phc2sys（仅 B2 有意义） | ADAPT-DEFERRED | 不得与 AoIP daemon 的 PTP 互斥冲突；B1 下不误启 ptp4l；失败可回滚 | 验：API.md §7、web/assets/app.js:1051；推断：新栈 mode owner |
| R23 | GET /api/aes67、/logs、/config | 服务状态+只读配置+journal 1..2000 行 | ADAPT-DEFERRED | 路径与字段（status/service）保持；端点配置只读展示来自实际渲染文件；日志行数上限保持 | 验：API.md §8、web/assets/app.js:1059-1119；推断：后端新 control |
| R24 | POST /api/aes67/start、/stop、/restart | systemd 操作 | ADAPT-DEFERRED | 新栈 owner 管理 aes67-daemon 生命周期；与 dante 互斥；失败恢复由 mode 状态机负责；不得静默成功 | 验：API.md §8、tspi-mode:327-365；推断：owner 换新栈 |
| R25 | GET /api/inferno、/logs、/config | 同上（Inferno 无统一配置文件） | ADAPT-DEFERRED | 保持"无独立服务"的说明语义；配置只读环境变量/ asound.conf 说明 | 验：API.md §8、web/assets/app.js:1059-1119；推断：后端新 control |
| R26 | POST /api/inferno/start、/stop、/restart | systemd 操作 | ADAPT-DEFERRED | Dante 模式由 statime+inferno 链路；失败不半启；不得回 C | 验：API.md §8、tspi-mode:296-325；推断：新栈 owner |
| R27 | GET /api/alsa | cards/pcm（实测通道）/hw_params/usb_devices（MOTU 07fd）/motu_present/告警 | ADAPT-DEFERRED | 新栈实际 ALSA 视图；稳定 ID 与 role scatter 显式；设备独占状态可见；不得假定 card 号 | 验：API.md §9、web/assets/app.js:1192-1200；推断：新 host 维护 |
| R28 | POST /api/alsa/test-tone | `{device,channel?}`；当前实现只校验 device（白名单），**channel 为文档字段、服务端未使用/静默忽略**（`web/src/routes/alsa.rs:34-38`）；内嵌 1k/48k/16bit WAV；mock 不执行 | ADAPT-DEFERRED | device 必填白名单；channel 必须显式决定：实现按通道播放或 400 unknown_field，禁止继续静默忽略；新 host 播放测试音、设备独占、默认低电平；401/400 保持；列入 §2.4 迁移白名单 | 验：API.md §9、web/src/routes/alsa.rs:34-38、web/assets/app.js:1178；推断：新 host 实现 |
| R29 | GET /api/motu/status | CueMix 在线状态/main_trim/direct/phantom；离线仍 200 online:false+reason | KEEP-DEFERRED | MOTU 控制面保留（final insert/主衰减/快照）；离线语义保持；不得因新栈改变用户 Toslink 直通 | 验：API.md §9.1、web/assets/app.js:1227-1259、PROTOCOL-REPORT:6,14 |
| R30 | GET /api/motu/snapshot | 结构化摘要；`?raw=1` 带 datastore；>512KiB 省略正文并标记 | KEEP-DEFERRED | 字段/上限保持；raw 省略规则保持 | 验：API.md §9.1、web/assets/app.js:1345-1360 |
| R31 | POST /api/motu/monitor | `{trim_db:0..=100,confirm:true}`；写 kMainTrim；写后校验+失败回滚 | KEEP-DEFERRED | confirm 缺省 400；越界 400；失败 500 WriteReport 保持；不得无确认写 | 验：API.md §9.1、web/assets/app.js:1279 |
| R32 | POST /api/motu/direct-mute | final insert；280 格差异写；主机输入 18..29 不写；恢复用快照 | KEEP-DEFERRED | 校准 insert 模式必须可一键关闭直通并留证；候选/主机输入不改语义；失败回滚 | 验：API.md §9.1、web/assets/app.js:1307 |
| R33 | POST /api/motu/restore | `{snapshot_id?,confirm:true}`；恢复 MIXER_KEYS（不含 kMainTrim）；404/400 | KEEP-DEFERRED | 缺省最近快照；id 不存在 404；无快照 400；写后校验 | 验：API.md §9.1、web/assets/app.js:1321 |

### 3.5 channelmap 面（R34–R47）

| ID | 方法/路径 | 现状与来源 | 处置 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- | --- |
| R34 | GET /api/channelmap | view：document/effective_roles/input_roles/validation/presets/probe/profiles/fir_files/mode_off/mock | ADAPT-DEFERRED | 字段保持；document 来自新栈唯一 active 文件；validation 由新栈权威校验器给出（不得本地放行） | 验：API.md §10、web/src/routes/channelmap.rs:104-107、web/assets/app.js:1501 |
| R35 | POST /api/channelmap/draft | `{input_layout,output_layout,profile_name?,capture_channels?,playback_channels?}` 生成显式 roles+sources 草稿 | ADAPT-DEFERRED | 未知布局 400；显式 N/O；不实现上混；stereo 默认只 FL/FR | 验：API.md §10、web/assets/app.js:1574,1640,1989；推断：新栈生成 |
| R36 | POST /api/channelmap/validate | 委托 `tspi-channelmap validate --json`；CLI 不可用 503；不得本地规则放行（mock 除外） | ADAPT-DEFERRED | 新栈权威校验器（T25/T13）同语义：error/warning 分级、未知字段 rc=2、未知 type 拒收；503→新栈 unavailable | 验：API.md §10、web/assets/app.js:2031；推断：校验器换新栈 |
| R37 | PUT /api/channelmap | 物化 roles+sources 保存 active；force=true 允许草稿 | ADAPT-DEFERRED | 严格 schema；保存与 apply 分离；备份旧文件；不得自动 apply | 验：API.md §10、web/assets/app.js:2042 |
| R38 | POST /api/channelmap/apply | 保存→校验→apply --mode；失败回滚信息（如 exit 9） | ADAPT-DEFERRED | prepare→commit actual version ack；设备宽度校验；stale 拒绝；失败保留 last-good 并返回证据 | 验：API.md §10、web/assets/app.js:2058；推断：新栈 apply 语义 |
| R39 | POST /api/channelmap/rollback | `tspi-channelmap rollback` | ADAPT-DEFERRED | 回滚到自研 last-good 图/coeff（不是回 C）；回滚后 actual version 可查 | 验：API.md §10、web/assets/app.js:2075；推断：last-good 由新栈 |
| R40 | GET /api/channelmap/render | 渲染结果文本；CLI 不可用 503；mock 读文件 | ADAPT-DEFERRED | 渲染来自新栈 graph/target；不得读旧 C 文件冒充；文本格式可不同但需 UI 同步 | 验：API.md §10、web/assets/app.js:2136；推断：渲染器换新栈 |
| R41 | GET/POST /api/channelmap/profiles | 列表/save `{name,document?}` | ADAPT-DEFERRED | profile 目录与命名规则保持；坏 document 400；不自动 apply | 验：API.md §10、web/assets/app.js:2097 |
| R42 | POST /api/channelmap/profiles/load、/delete | `{name}`；load 覆盖 active（备份）不自动 apply；不存在 404 | ADAPT-DEFERRED | 语义保持；load 后 actual active 可查；delete 不存在 404 | 验：API.md §10、web/assets/app.js:2106,2115 |
| R43 | GET /api/channelmap/export | `?name=` 导出 YAML | ADAPT-DEFERRED | 导出内容与 active/命名一致；不得导出旧 C 渲染文件 | 验：API.md §10、web/assets/app.js:1626,2132 |
| R44 | POST /api/channelmap/import | `{yaml,name?}`；name 空写 active | ADAPT-DEFERRED | 严格 schema/未知字段拒收；大小上限；导入不自动 apply | 验：API.md §10、web/assets/app.js:2125 |
| R45 | GET/POST /api/channelmap/fir | FIR 列表/上传 base64 ≤16MiB；解析采样率/taps/群延迟 | ADAPT-DEFERRED | 上传校验 WAV/格式；**有效支撑 >4096 需 T56 资源 profile 准入（extended 8192/16384 须 RAM/deadline/延迟准入），>16384 运行期拒收**；与当前 fs/role/预算不符拒收；不得静默截断；Web UI 的“>4096 实验/>8192 强警告”只是界面标签，不是运行准入依据 | 验：API.md §10、WBS §6、WBS T22/T56、T04 §7.2；推断：新栈准入 |
| R46 | POST /api/channelmap/test-tone | `{role?,output_channel?,kind:"tone"/"sweep",duration_ms?}` | ADAPT-DEFERRED | 新 host 角色/通道测试音；设备独占；未知 role/通道 400；不得在播放中抢设备 | 验：API.md §10、web/assets/app.js:2089；推断：新 host 实现 |
| R47 | GET/PUT /api/channelmap/input-mode | S1 内容模式：GET 读运行态+覆盖；PUT 写覆盖（auto/stereo/multichannel，force-* 别名） | **RETIRE（P1，状态码冻结）** | P1 不实现内容自动上混：**GET 200 `{"status":"not_supported","reason":"p1-no-content-upmix"}`**（保留轮询不误报，不得 200 假 auto）；**PUT 400 `{"error":{"code":"not_supported",...}}`**；Web UI 同变更隐藏/禁用；禁止启动内容 daemon 或 fader 切上混；此两条为 X15 一般 410 规则下的唯一例外（见 §2.1/§9 X15） | 验：API.md §10、web/src/routes/channelmap.rs:109-127、web/src/channelmap.rs:1229-1334,1757-1797；DEC §6；推断：P1 拒绝码 |

### 3.6 P1 新增与横切（R48–R51）

| ID | 方法/路径 | 现状与来源 | 处置 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- | --- |
| R48 | ADD 校准 job REST（start/status/cancel/apply/retest/rollback） | 当前无；S1 手工 calibrate 流程 | ADD | job 状态至少 queued→preflight→sweep/capture→quality_check→fit_P→solve_G→compile→prepared→apply→remeasure→independent_holdout→complete/failed/cancelled/rolled_back（DEC §2）；设备独占互斥（DEC §2）；cancel 安全停扫频；apply 实际 ack+version；retest/rollback 可查；失败只 last-good/mute；报告含 raw/hash/版本（WBS §5） | 验：DEC §2、WBS T60/T70/§5；推断：路径与响应字段由 T02/T42 冻结（Q06） |
| R49 | ADD 新栈 runtime/control REST（status/volume/mute/target/coeff/version） | 当前分散在 camilladsp/audio 面；UI 依赖 | ADD | 必须覆盖 Web 日常：输入/路由/volume/mute/状态/目标/系数/校准入口；状态响应含 actual graph/coeff/config version 与 hash；volume 范围 -150..+50（继承）；mute 幂等；prepare/commit 分离；冲突 409 | 验：DEC §6、WBS T42/T44/T70；推断：具体路径 T42 |
| R50 | 错误 envelope 与 API 版本（横切） | 既有统一 `{"error":{"code","message"}}`；HTTP 400/401/403/404/409/429/500/503（API.md §1、web/src/error.rs:44-68）；无 API 版本字段 | KEEP（版本策略见 X14） | 所有 REST（含 serde 提取失败）必须返回同一 envelope；版本使用**可选**请求头 `X-TSPI-API-Version`（缺省=v1），仅显式不兼容版本才 400 `unsupported_api_version`；响应头回 `X-TSPI-API-Version: 1`；状态码集合以 API.md §1 表为准（不含 422，见 X03）；安全头保持 | 验：API.md §1、web/src/error.rs:44-68,92-101、web/src/routes/mod.rs:132-151；推断：版本头 ADD |
| R51 | 静态资源与 fallback | `/`、`/index.html`、`/style.css`、`/app.js`、`/favicon.*`、`/test-tone.wav` 公开；未知路径 404 纯文本；`/api` 外 fallback 到静态 | KEEP（单一处置） | 同源无 CDN；CSP 允许同源脚本；T70 新 UI 可更新内容但不得引入外部资源；未知 `/api/*` 必须 JSON 404（X01）；静态路径保持给旧 UI 迁移期用 | 验：API.md §11、web/src/assets.rs:32-48、web/src/routes/mod.rs:125；未测：`/api/*` 未知路径实际 fallback 行为（§13.3 未测项 6） |

### 3.7 R 矩阵 ↔ rest.rs 路由落地对照（F-21，机器可读）

> **唯一机器可读对照**：本表把 R01-R51 逐行绑定到 `crates/control/src/rest.rs` 的 `RestService::routes()`；本表 51 行是绑定表，**不改变 §2.3 的 163 矩阵行计数**（R 行计数口径见 §13.6 #2）；`crates/control/tests/T42/matrix_routes.rs` 在 `cargo test -p khaen-control rest` 中自动核对「处置列 ↔ 落地状态 ↔ 路由存在性 ↔ 无孤儿路由 ↔ R01-R51 各一次」，并带 5 条负向用例。改本表或 `routes()` 必须同步，否则测试红。
>
> 落地状态取值：`ROUTED`=已注册且由 handler 实现；`RETIRED_ROUTE`=按 RETIRE 语义注册（R47 唯一例外）；`CROSS_CUTTING`=无独立路径的横切语义；`DEFERRED`=P1 不落地，**不得注册路由**（`/api/*` 按 X01 JSON 404，不得空壳 200/假成功）。处置列必须与状态一致：`*-DEFERRED` ⇔ `DEFERRED`，`RETIRE` ⇔ `RETIRED_ROUTE`，R50/R51 为 `KEEP` + `CROSS_CUTTING`，其余 `ROUTED` ⇔ KEEP/ADAPT/ADD。

| ID | 处置 | 落地状态 | 方法与路径（`RestService::routes()`） | 处理函数 | 测试 | 未落地理由 / owner |
| --- | --- | --- | --- | --- | --- | --- |
| R01 | KEEP | ROUTED | GET /healthz | RestService::handle_healthz | t42_rest_healthz_is_anonymous_and_has_r01_fields | — |
| R02 | KEEP | ROUTED | GET /api/auth/status | handle_auth_status | t42_rest_auth_status_anonymous_fields | — |
| R03 | KEEP | ROUTED | POST /api/auth/setup | handle_auth_setup | t42_rest_auth_setup_validates_password_and_logs_in | — |
| R04 | KEEP | ROUTED | POST /api/auth/login | handle_auth_login | t42_rest_auth_login_failure_lockout_and_recovery | — |
| R05 | KEEP | ROUTED | GET /api/auth/session | handle_auth_session | t42_rest_auth_session_and_logout | — |
| R06 | KEEP | ROUTED | POST /api/auth/logout | handle_auth_logout | t42_rest_auth_session_and_logout | — |
| R07 | ADAPT-DEFERRED | DEFERRED | — | — | — | 聚合依赖 system/services/ptp/aoip/alsa/power 各域后端；control 核无数据源，T70 新 UI 用 R49；owner T26/T43/T44/T70 |
| R08 | KEEP-DEFERRED | DEFERRED | — | — | — | hostname/kernel/cpu/memory/network 主机视图属 T26 host（control 核无 OS 依赖，rt-stack/README §2）；owner T26/T70 |
| R09 | KEEP-DEFERRED | DEFERRED | — | — | — | reboot/shutdown 危险操作需部署配置与 systemd 权限，属 T26/部署层；owner T26/T70 |
| R10 | ADAPT | ROUTED | GET /api/mode | handle_legacy_mode_get | t42_rest_legacy_mode_get_has_r10_fields | — |
| R11 | ADAPT | ROUTED | PUT /api/mode | handle_legacy_mode_put | t42_rest_mode_switch_returns_202_with_actual_ack | — |
| R12 | ADAPT | ROUTED | GET /api/mode/script-status | handle_legacy_mode_script_status | t42_rest_script_status_is_available_false_not_500 | — |
| R13 | ADAPT | ROUTED | GET /api/audio/format | handle_audio_format_get | t42_rest_audio_format_get_limits_and_actual | — |
| R14 | ADAPT | ROUTED | PUT /api/audio/format | handle_audio_format_put | t42_rest_audio_format_put_applies_through_prepare_commit | — |
| R15 | ADAPT-DEFERRED | DEFERRED | — | — | — | Camilla 只读面：P1 无 C 进程、旧 web 已标退役栈镜像（F-04）；T70 用 R49 runtime status；owner T70 |
| R16 | ADAPT-DEFERRED | DEFERRED | — | — | — | start/stop/restart 无 engine 后端 API；旧 web 启停路由已删（F-04）；新栈准备/提交走 R49 prepare/commit；owner T26/T44/T70 |
| R17 | ADAPT-DEFERRED | DEFERRED | — | — | — | graph/config 目录列表无新栈文件后端（coeff 列表在 R49）；不把 Camilla YAML 当运行时真值；owner T25/T70 |
| R18 | ADAPT-DEFERRED | DEFERRED | — | — | — | Camilla YAML→plan 转换/校验属 T13/T25 适配层，未接入 control；owner T13/T25/T26 |
| R19 | ADAPT-DEFERRED | DEFERRED | — | — | — | 新栈音量入口为 R49 PUT /api/runtime/volume（已落地）；旧 Camilla 路径 P1 无消费者；owner T70（迁移期） |
| R20 | ADAPT-DEFERRED | DEFERRED | — | — | — | PTP 运行态归 T43/T44（Q05）；control 核无 ptp4l/phc2sys 后端；owner T43/T44 |
| R21 | ADAPT-DEFERRED | DEFERRED | — | — | — | PTP 配置写/回滚需 linuxptp 文件与 systemd 权限；owner T43/T44/T26 |
| R22 | ADAPT-DEFERRED | DEFERRED | — | — | — | ptp4l/phc2sys 启停与 AoIP 互斥归 mode owner T44/T43 |
| R23 | ADAPT-DEFERRED | DEFERRED | — | — | — | aes67-daemon 状态/日志/配置渲染归 T43 + systemd 执行层；owner T43/T26 |
| R24 | ADAPT-DEFERRED | DEFERRED | — | — | — | AoIP 服务生命周期归 T44 mode 状态机（唯一 owner）；owner T44/T43 |
| R25 | ADAPT-DEFERRED | DEFERRED | — | — | — | Inferno 是 ALSA 插件、无独立服务，只读说明归 T43/T70 |
| R26 | ADAPT-DEFERRED | DEFERRED | — | — | — | Dante（statime+inferno）启停归 T44 mode 状态机；owner T44/T43 |
| R27 | ADAPT-DEFERRED | DEFERRED | — | — | — | ALSA cards/pcm/hw_params 视图归 T26 host ALSA 后端（T17）；control 核无 ALSA 依赖 |
| R28 | ADAPT-DEFERRED | DEFERRED | — | — | — | 测试音播放+设备独占归 T26 host（T17/T43）；control 核无设备；owner T26 |
| R29 | KEEP-DEFERRED | DEFERRED | — | — | — | MOTU CueMix 控制面客户端未在新栈实现（final insert/主衰减/快照）；owner T70/T74 |
| R30 | KEEP-DEFERRED | DEFERRED | — | — | — | 同 R29：CueMix snapshot 只读摘要后端未实现；owner T70/T74 |
| R31 | KEEP-DEFERRED | DEFERRED | — | — | — | 同 R29：kMainTrim 写入/校验/回滚后端未实现；owner T70/T74 |
| R32 | KEEP-DEFERRED | DEFERRED | — | — | — | 同 R29：final insert 直通关闭/快照恢复后端未实现；owner T70/T74 |
| R33 | KEEP-DEFERRED | DEFERRED | — | — | — | 同 R29：MIXER_KEYS 恢复后端未实现；owner T70/T74 |
| R34 | ADAPT-DEFERRED | DEFERRED | — | — | — | channelmap v2 文档/校验视图归 T25/T57；control 只承载 target/coeff 元数据（R49） |
| R35 | ADAPT-DEFERRED | DEFERRED | — | — | — | draft 生成需 channelmap v2 模型（T25）；owner T25/T70 |
| R36 | ADAPT-DEFERRED | DEFERRED | — | — | — | validate 委托新栈权威校验器（T25/T13），未接入 control；owner T25/T13 |
| R37 | ADAPT-DEFERRED | DEFERRED | — | — | — | PUT active channelmap 物化/备份归 T25/T57；owner T25/T57 |
| R38 | ADAPT-DEFERRED | DEFERRED | — | — | — | channelmap apply 的 prepare→commit actual ack 归 T26/T25；owner T26/T25 |
| R39 | ADAPT-DEFERRED | DEFERRED | — | — | — | last-good 图/coeff 回滚归 T26/T20；owner T26/T25 |
| R40 | ADAPT-DEFERRED | DEFERRED | — | — | — | 渲染器换新栈 graph/target 归 T25/T57；owner T25/T57 |
| R41 | ADAPT-DEFERRED | DEFERRED | — | — | — | channelmap profile 目录/命名规则归 T25/T57；owner T25/T57 |
| R42 | ADAPT-DEFERRED | DEFERRED | — | — | — | profile load/delete 归 T25/T57；owner T25/T57 |
| R43 | ADAPT-DEFERRED | DEFERRED | — | — | — | channelmap export 归 T25/T57；owner T25/T57 |
| R44 | ADAPT-DEFERRED | DEFERRED | — | — | — | channelmap import 严格 schema 归 T25/T57；owner T25/T57 |
| R45 | ADAPT-DEFERRED | DEFERRED | — | — | — | FIR 列表/上传/资源准入归 T26 host + T56/T25；owner T26/T56/T25 |
| R46 | ADAPT-DEFERRED | DEFERRED | — | — | — | 角色/通道测试音归 T26 host（T17）；owner T26 |
| R47 | RETIRE | RETIRED_ROUTE | GET /api/channelmap/input-mode; PUT /api/channelmap/input-mode | handle_input_mode_get; handle_input_mode_put | t42_rest_input_mode_is_retired_with_frozen_codes | — |
| R48 | ADD | ROUTED | GET /api/calibration/jobs; POST /api/calibration/jobs; GET /api/calibration/jobs/{id}; POST /api/calibration/jobs/{id}/cancel; POST /api/calibration/jobs/{id}/apply; POST /api/calibration/jobs/{id}/retest; POST /api/calibration/jobs/{id}/rollback; GET /api/calibration/settings; POST /api/calibration/settings; POST /api/calibration/settings/cal-file; GET /api/calibration/status; POST /api/calibration/apply; POST /api/calibration/clear; POST /api/calibration/run | handle_cal_jobs_list; handle_cal_jobs_start; handle_cal_job_get; handle_cal_job_cancel; handle_cal_job_apply; handle_cal_job_retest; handle_cal_job_rollback; handle_cal_settings_get; handle_cal_settings_post; handle_cal_settings_cal_file; handle_cal_status_get; handle_cal_apply; handle_cal_clear; handle_cal_run | t42_rest_calibration_start_status_cancel_flow; t42_rest_calibration_apply_and_rollback_actual_acks; t42_rest_calibration_settings_roundtrip; t42_rest_calibration_level_delay_lanes_and_pending_501 | — |
| R49 | ADD | ROUTED | GET /api/runtime/status; GET /api/runtime/config; PUT /api/runtime/config; POST /api/runtime/prepare; POST /api/runtime/commit; POST /api/runtime/abort; GET /api/runtime/volume; PUT /api/runtime/volume; GET /api/runtime/mute; PUT /api/runtime/mute; GET /api/runtime/mode; PUT /api/runtime/mode; GET /api/runtime/target; PUT /api/runtime/target; GET /api/runtime/coeff; PUT /api/runtime/coeff | handle_runtime_* | t42_rest_runtime_status_reports_actual_identity; t42_rest_prepare_commit_applies_actual_version_and_is_idempotent; t42_rest_volume_actual_ack_and_bounds; t42_rest_target_get_reports_actual_and_desired; t42_rest_coeff_get_lists_available_versions | — |
| R50 | KEEP | CROSS_CUTTING | — | — | t42_rest_api_version_header_is_optional_and_rejects_incompatible; t42_rest_unknown_paths_are_json_404; t42_rest_json_strictness_duplicate_nan_depth_and_types | 横切：无独立路径；统一 envelope/版本头由全部 handler 的 RestError/RestResponse 实现 |
| R51 | KEEP | CROSS_CUTTING | — | — | t70_web_static_routes_have_ui_security_headers; t42_rest_unknown_paths_are_json_404 | 横切：静态资源/fallback 归 T70 web 层（web.rs）；rest 只保证 /healthz 与 /api/* JSON 404/405 |

> 汇总：ROUTED 13 行（R01-R06、R10-R14、R48、R49）+ RETIRED_ROUTE 1 行（R47）+ CROSS_CUTTING 2 行（R50、R51）= 16 行已落地/横切；`ADAPT-DEFERRED` 28 行 + `KEEP-DEFERRED` 7 行 = 35 行 P1 不落地（`/api/*` 未知路径 X01 JSON 404）。DEFERRED 行的目标语义（P1 要求列）保留为后续落地验收口径，不得当成已支持。

---

## 4. WS 兼容矩阵（W01–W22）

> 背景：现有 `khaen-audio-web` 以 CamillaDSP websocket 客户端形式读取 live 状态/配置并写音量；S1 upmix daemon 另用两条命令。P1 无 C 进程，但 **T24 要求在新栈内实现"WS 必需子集"**（WBS T24），使 Web/未来消费者按稳定语义工作。下表的"要求"即 T24 的验收口径，具体字段名沿用既有消费者已编码的形态。
> 说明（F-32）：W01–W22 是 Camilla/S1 兼容面；**W23–W25 是 P1 ADD 的显式 prepare/commit/abort 命令**（非 Camilla 原生），三者必须与 REST R49 使用同一套 actual-ack 判据，真实 host 路径在 T26 接线前标 NOT_RUN。

| ID | 命令/协议点 | 现状与来源 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- |
| W01 | 传输帧 | JSON 文本帧；web 客户端也接受 Binary 当 UTF-8；Ping/Pong 忽略；Close 视为错误 | 只接受文本 JSON；Binary 帧显式报错（不静默当文本）；Ping/Pong 正常响应；非法 UTF-8 拒绝 | 验：web/src/camilla.rs:299-314；推断：新栈更强约束 |
| W02 | 请求形态 | 字符串命令（如 `"GetState"`）或单键对象（如 `{"SetVolume":-12.0}`）；命令名取首键 | 两种形态都支持；对象多键视为非法命令（拒绝，不猜） | 验：web/src/camilla.rs:3-7,238-244 |
| W03 | 回复形态 | `{"<命令>":{"result":"Ok","value":...}}`；错误 result 为字符串或明细对象；客户端按命令名取回复 | 必须以请求命令名为键回复；result 非 Ok 时给可读错误；不得回复其他键/空对象导致客户端超时 | 验：web/src/camilla.rs:259-269,877-889 |
| W04 | 连接与顺序 | 每批命令新建连接，逐条发送并等待对应回复；超时 200..30000ms 可配 | 单连接支持顺序多命令；每命令必须有回复；服务端要有界队列/超时；不要求推送/订阅 | 验：web/src/camilla.rs:279,287-325；推断：新栈实现 |
| W05 | GetVersion | 返回版本字符串 | 返回新栈 WS 协议版本号（不是 Camilla 版本）；用于客户端能力判断 | 验：web/src/camilla.rs:436,460；推断：版本语义新栈定义 |
| W06 | GetState | 字符串状态；web 仅把 `"Running"` 视为运行；停止原因区分错误 | 状态枚举固定（至少 Running/Paused/Inactive/Starting/Error）；Running 仅在真实出流后返回；不得乐观置 Running | 验：web/src/camilla.rs:44-52,461；推断：枚举新栈冻结 |
| W07 | GetStopReason | CaptureError/PlaybackError/CaptureFormatChange/PlaybackFormatChange 等被视为错误 | 返回稳定原因码；正常停止不得报错误原因；错误原因可映射到 REST status | 验：web/src/camilla.rs:47-52,462；推断：新栈原因集 |
| W08 | GetConfigFilePath、GetConfigTitle | 当前配置文件路径/标题 | 返回新栈 graph/manifest 路径与标题（可为逻辑标识）；路径不得暴露无关敏感文件 | 验：web/src/camilla.rs:439-440,463-464；推断：新栈语义 |
| W09 | GetVolume、GetMute | 音量 f64 dB、静音 bool | 返回 actual 值；范围 -150..+50；mute 与 volume 不互相覆盖；写入后读回一致 | 验：web/src/camilla.rs:441-442,465-466；API.md §6；推断：新栈 actual 语义 |
| W10 | GetCaptureRate | capture 采样率 u64 | 返回实际输入采样率（48k/96k 等），不是配置意图值 | 验：web/src/camilla.rs:443,467；推断：actual 要求 |
| W11 | GetClippedSamples | 累计削波样本 u64 | 返回新栈 I/O 区累计 clip 计数；单调递增；重启归零需在 REST 标明 epoch | 验：web/src/camilla.rs:444,468；DEC §5；推断：新栈计数 |
| W12 | GetProcessingLoad | 处理负载 f64 | 返回有界统计（如 busy 占比），定义口径并文档化；不得用 spin 伪造 | 验：web/src/camilla.rs:445,469；DEC §4；推断：口径新栈定 |
| W13 | GetBufferLevel | 缓冲水位 i64（帧） | 返回 playback 队列水位；单位帧；溢出/欠载时数值语义明确 | 验：web/src/camilla.rs:446,470；推断：新栈 |
| W14 | GetRateAdjust | 速率调整 f64 | 返回唯一 clock servo 的 ppm/调整量；不暴露第二时钟闭环 | 验：web/src/camilla.rs:447,471；DEC §5；推断：新栈唯一 servo |
| W15 | GetConfig | 返回当前配置 YAML 字符串；WS 不可用时 web 回退读文件 | 返回当前 graph/manifest 文档（新栈自有 schema，带版本/hash）；内容不含密钥；格式变更需 REST/UI 同步 | 验：web/src/camilla.rs:524-556；推断：文档 schema 新栈定 |
| W16 | SetConfigJson | 传 JSON 字符串（由 YAML 转 JSON）；成功才返回 Ok；失败明细；web 失败回退 systemd 重启 | **必须 prepare 校验+资源准入后再 commit，真正生效后返回 Ok**（T24"prepare commit 实际 ack"）；失败不得部分应用；返回 new version/hash；不得 C 重启兜底 | 验：web/src/camilla.rs:327-335,337-412；WBS T24；DEC §5；推断：新栈语义 |
| W17 | SetVolume | f64；范围 -150..+50；返回实际值或 {volume} | 越界显式错误；返回 actual dB；与 mute 状态一致；写后 GetVolume 必须一致 | 验：web/src/camilla.rs:590-608；推断：新栈 |
| W18 | GetCaptureSignalRms | 返回 `value` 数组（按采集通道索引的 dB 值）；S1 对缺失值按 -240 处理 | T24 列为必需兼容项：数组长度=采集通道数；单位 dBFS；静音可用有限负值（如 -150/-240 口径固定并文档化）；不得返回 NaN/Inf | 验：tspi-upmix:1691-1704；WBS T24；推断：口径新栈冻结 |
| W19 | SetFaderVolume | `[fader_id:int, volume_db:float]`；S1 用 fader 1/2 切 Aux 路径；失败抛错 | T24 列为必需兼容项：支持已分配 fader id 的音量写入；未知 id 明确错误；不得用 fader 绕过保护/限幅；P1 无内容上混，不得把 fader 当"切上混路径" | 验：tspi-upmix:1707-1717；WBS T24；DEC §6；推断：fader 语义新栈定 |
| W20 | 未知命令 | 现有客户端只处理已知命令；未回复会超时 | 必须回复同键错误（code=UnknownCommand/unsupported），连接可继续；不得静默/挂起（客户端有超时但不可依赖） | 验：web/src/camilla.rs:259-269；tspi-upmix cmd 超时：1666-1678；推断：新栈契约（Q08） |
| W21 | 非法请求 | 未验证（现有客户端不发） | 非 JSON/空命令/类型错误：返回错误或关闭并给原因码；不得 panic；单帧大小上限；错误不泄露内部路径 | 推断：新栈契约（未测） |
| W22 | 无 GetXruns | CamillaDSP 4.x 无该命令，xrun 由 journal+StopReason 推断 | 新栈不得假设/伪造 GetXruns；xrun/under/over 通过 REST 状态或计数提供；WS 兼容子集不扩该命令 | 验：API.md §6；web/src/camilla.rs:6-7,55-84 |
| W23 | PrepareConfigJson | P1 ADD（非 Camilla 原生；`crates/compat/src/ws.rs` 的 `P1_ADDED_COMMANDS` 已实现）。只做 schema/字段/长度/taps/rate/role/精度/hash 校验 + 容量准入，**不得改变音频输出**；成功返回 `prepared_plan_version/graph_id/config_sha256/expected_workers` 凭据 | ADD：prepare 与 commit 分离；无 pending 时 commit/abort 返回 `not_prepared`；坏配置返回 `prepare_rejected` 且零副作用；与 T42 `/api/runtime/prepare` 同一判据 | 验（库内）：ws.rs:143-151（P1_ADDED_COMMANDS）、:2047（prepare_config）；`cargo test -p khaen-compat` W16 用例；**未测：真实 host/板端 NOT_RUN**（T26） |
| W24 | CommitConfig | P1 ADD（`ws.rs` 已实现）。必须等 prepared plan 出现在 ≥1 个完整 block 且 `ack_mask=ACK_MASK(expected_workers)` 后再返回 Ok；graph_id/config_sha256/version 与 prepare 一致；host 声明 `rolled_back` 即失败 | ADD：成功返回 `applied/new plan_version/blocks_committed/ack_mask`；任何一项不满足返回 `commit_not_acked` 并 abort/last-good，**绝不假 Ok**；与 T42 `/api/runtime/commit`/`ws::validate_commit_ack` 同一函数 | 验（库内）：ws.rs:1350（`validate_commit_ack`）、:2083（commit_config）；T24 W16 用例；**未测：真实 host/板端 NOT_RUN**（T26） |
| W25 | AbortConfig | P1 ADD（`ws.rs` 已实现）。安全丢弃已 prepared 但未 commit 的 plan，保留 last-good；无 pending 时返回 `not_prepared` | ADD：abort 不改变已生效输出；重复 abort 幂等（`already_invalidated`）；与 T42 `/api/runtime/abort` 语义一致 | 验（库内）：ws.rs:2118（abort_config）；T24/T42 abort 用例；**未测：真实 host/板端 NOT_RUN**（T26） |

---

## 5. YAML/JSON 文件契约矩阵（Y01–Y24）

> 说明：channelmap/layout-presets 以板端 Python `tspi-channelmap` 为唯一权威（API.md §10）；校准以 `calibration-spec/CONTRACT.md` 为权威；本表给新栈 adapter（T13/T25/T57）的兼容要求。

| ID | 文件/字段 | 现状契约与来源 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- |
| Y01 | channelmap v2 顶层键 | 白名单 `schema/version/profile/input/output/remap/bass_management`；未知键=error（"v2 严格校验"） | 新栈解析必须同样严格；未知键拒收并报路径；不得丢弃后继续 | 验：tspi-channelmap:36,78-85；API.md §10|
| Y02 | schema/version | `schema=tspi-channelmap/v2`、`version=2`；不匹配 rc=2 | 精确匹配；新版本另立 schema；旧版只读兼容 | 验：tspi-channelmap:164-167；API.md §10|
| Y03 | input 块 | 键 `layout/channels/presets/probe_order`；channels 可 auto；apply 优先实测通道数 | 保留键语义；未知布局/缺布局报错；N 可变；不实现上混 | 验：tspi-channelmap:38；API.md §10|
| Y04 | output 块 | 键 `device/channels/layout/roles`；channels 可 auto；profile 值作 fallback | 保留；输出宽度与实测设备校验；未知 layout 拒收 | 验：tspi-channelmap:39；API.md §10|
| Y05 | roles 字段与边界 | `id/label/enabled/mute/outputs/sources/gain_db/delay_ms/polarity/highpass/lowpass/peq/fir/link_to`；gain -60..+12、delay 0..200ms、polarity normal/invert、HP/LP 20..fs/2、阶数 2/4/6/8、PEQ≤32 | 保留全部字段语义与边界；越界拒收（rc=3）；未知字段拒收；多路 fan-out 支持 | 验：tspi-channelmap:43-45；API.md §10|
| Y06 | PEQ 段 | `{freq,q,gain_db,type?}`；type∈{peaking(缺省),low_shelf,high_shelf}；peaking q 0.1..10；shelf q=RBJ slope S∈0.1..1.0；未知 type rc=2；渲染 shelf slope=12*q | 保留；不得丢 type；不得把 shelf 当 peaking；未知 type 整份拒收 | 验：tspi-channelmap:46-50,395-411；API.md §10；CONTRACT.md:137-139 |
| Y07 | FIR 引用 | `{path,samplerate,channel?}`；path 相对 profile 或绝对；fs 必须匹配当前模式；taps>4096 实验、>8192 强警告（Web UI 标签） | 保留键；未知键/非法路径/缺 fs（无法解析时）拒收；与当前 fs 不一致拒收；**运行准入按 R45：>4096 需 T56 profile 准入（8192/16384 extended），>16384 运行期拒收**；UI 的“实验/强警告”不是准入依据；不得静默截断 taps | 验：tspi-channelmap:51,114-133；API.md §10；WBS §6；推断：新栈准入 |
| Y08 | remap | `{preset,routes,drop_unused,silence_unmapped}`；route `{src,dest,gain_db,mute,mode}`；mode∈direct/fold/duplicate/mix/drop/silence；sources 与 remap 不能同时非空 | 保留；未知 preset/角色拒收；sources 为最终矩阵时 remap 仅生成用，不得双计 | 验：tspi-channelmap:40-42,66,457-502；API.md §10|
| Y09 | bass_management | `enabled/lfe_role/lfe_to/lfe_gain_db/redirect{mains_highpass_hz,...}`；enabled 时 lfe_to/targets 引用的 SUB 角色 sources 必须为空 | 保留；未知角色拒收；渲染器接管 LFE+redirect 与 mains HP 的规则保持；P1 不新增"节目上混" | 验：tspi-channelmap:52-55,557；API.md §10|
| Y10 | layout-presets v2 | `schema=tspi-layout-presets/v2`、input_layouts/output_layouts/remap_presets；4.1.2 角色 outputs 见 Y11 | 严格 schema；只读；新栈必须解析而非硬编码；与 Web 内嵌副本 drift 检查机制保留（现有 cargo test） | 验：layout-presets.yml:5,116-229；tspi-channelmap:56,103-111；API.md §10|
| Y11 | 4.1.2 唯一 role→DAC | 0-based FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7、ch6 空；8ch 容器 7 物理角色 | 新栈唯一映射来源；须校验实际设备宽度；禁止用 Web 模板 SUB4；低带 dense 0..4→DAC 0/1/2/3/7 另表保存 | 验：layout-presets.yml:219-229；WBS §3；DEC §1；T57 |
| Y12 | calibration v1 字段/范围 | 顶层 schema/sample_rate/reference_index/layout/created_unix/tool/version/note/measurement/channels；delay 0..200ms 且 min=0；polarity normal/invert；gain -12..+6；filters≤32；freq 20..min(20k,fs/2)、q、gain -24..+24；未知 type 拒收 | 新栈 T13 必须接受 v1（只读）；范围/唯一性/参考检查按 CONTRACT §5；错误拒收、warning 记录；不得改旧文件 | 验：CONTRACT.md:26-90,181-196；WBS T13|
| Y13 | calibration v1.1 | v1 基础上 type 允许 low_shelf/high_shelf；peaking q 有限≥0.1（可>10）；shelf q=S∈[0.1,1.0]；未知 type 整份拒收；v1 消费方读不了含 shelf/q>10 的文件 | T13 必须接受 v1.1 并保留 type；渲染映射 slope=12*S；不得丢 type/降级 peaking | 验：CONTRACT.md:137-139,245-258；API.md §10|
| Y14 | 校准未知项策略 | 未知顶层/通道字段=warning（保留扩展性）；未知 filter type=error；schema 不匹配=error | 新栈：语义未知（type/单位/参考/超预算）一律拒收；纯扩展字段可 warning 但不得参与应用；warning 必须上报到 job 报告 | 验：CONTRACT.md:196,89,137；已执行 validator 复核（见 §13.2） |
| Y15 | 校准→渲染映射 | delay_ms=delay_samples*1000/fs；polarity=XOR(render, calib)；gain 相加；PEQ 追加且 type 原样；role 不存在拒收；delay 越界拒收；不双乘 curve | 新栈等价实现（映射到自有 graph）；极性只信校准字段；G 只模态残差、curve 不双乘（WBS §5）；apply 前校验、apply 后可复测 | 验：CONTRACT.md:141-163；WBS §5；DEC §2|
| Y16 | 校准生命周期/位置 | ` /etc/tspi-audio/calibration/` 历史只增不改、原子写、current 可选；文件不自动触发应用 | 新栈保留只增不改/原子写/current 选择；apply 需显式 job；每个 artifact 带 hash/version/role/stale 字段（T13 IR 草案） | 验：CONTRACT.md:211-222；WBS T13/§5|
| Y17 | target 导入草案（对齐 T04 现行 §6） | 字段/语义以 T04 §6（`rt-stack/spec/fir-toolbox.md`）为准：schema=tspi-target-draft/v1；必填 frequency_unit=Hz、magnitude_unit=dB、runtime_rate（必须=profile fs）、normalization=none/ref_1k/peak、interpolation=log_frequency_linear、edge_policy=hold/reject；有 phase 时必填 phase_unit=deg、phase_ref=none/absolute/delay_removed、unwrap_policy=unwrapped/auto；target_delay_samples 仅 phase_ref=delay_removed 必填（整数 0..200ms），absolute/none 必须缺省；reference_gain_db [-24,+24]；source_hash CSV 必填/ JSON 可选；x_ 未知字段 warning、其它未知字段 E_TGT_FIELD_UNKNOWN；**拒收码全集见 T04 §6.9（T04 v1.2-draft 为 28 码，随 T04 版本刷新；不写死行号/码数）** | T11 实现同规则；缺 phase 必须选明确设计 family 不猜零相位；20Hz/20k 端点另验 | 验：T04 §6（现行 v1.2-draft）；WBS §6 |
| Y18 | input-content v1 / input-mode v1 | 运行态字段见 §6；覆盖文件 `{schema,mode,updated_unix,updated_by}`，mode∈auto/stereo/multichannel（兼容 force-* 别名） | **P1 不消费**：字段只作 S1 兼容资料；新栈不得据此切上混；REST 端点显式拒绝（R47/C07） | 验：INPUT-MODE.md:10-47；API.md §10；web/src/channelmap.rs:1229-1243 |
| Y19 | 新栈 graph/manifest（ADD） | 当前为 CamillaDSP YAML（dante.yml/aes67.yml 模板）；P1 自有 IR/graph | 新增版本化 schema（version/hash/role/fs/taps/placement/P/phase/D/layout/limiters）；prepare/apply/stale 规则明确；不得把 Camilla YAML 当 P1 真值 | 验：DEC §2/§3；WBS §5；推断：T13/T42 冻结 |
| Y20 | AoIP/PTP 配置模板 | daemon.conf JSON（http 8080/rtsp 8854/rtp 5004/mcast 239.1.0.1/ptp_domain 0/status_file）；statime TOML（kebab-case、deny_unknown_fields）；ptp4l conf（不支持行尾注释） | 新栈要渲染/校验这些模板：未知键拒收；ptp4l conf 禁止行尾注释；interface 占位符必须替换；配置 hash 纳入 actual version | 验：daemon.conf:1-37；statime toml:1-29；ptp4l conf:1-30；aoip REPORT:21,30 |
| Y21 | CamillaDSP YAML | dante.yml/aes67.yml 模板；web 以 serde_yaml 校验并 WS 应用；仅开发参考 | P1 **仅开发 oracle**：不得作为运行时真值；可归档；新栈不依赖其字段 | 验：API.md §5/§6；DEC §0；audio/overlay/etc/camilladsp/*.yml |
| Y22 | schema 不匹配 | channelmap rc=2；presets 加载即拒；calibration schema error | 所有文件入口先校 schema/version；不匹配拒收；不得"尽量解析" | 验：tspi-channelmap:106-107,164-167；CONTRACT.md:185 |
| Y23 | 数值/单位校验 | 目标文件要求正频递增唯一、无 NaN；channelmap 数值范围；校准 delay 整数/极性枚举 | 新栈所有解析器：NaN/Inf/负数/超范围/混合单位/歧义语义一律拒收；错误带字段路径 | 验：WBS §6；CONTRACT.md:5,89；tspi-channelmap:88-95 |
| Y24 | IR/manifest 版本与 stale | 无现成契约（P1 新） | 每个 artifact 必须带 version/hash/role/fs/taps/placement、P/phase/D、stereo mask/目标 profile；input/fs/role/P/hash 变化即 stale 重算；坏集不生效 | 验：DEC §2；WBS §5；推断：T13/T60 冻结 |

---

## 6. 输入内容检测契约与 P1 处置（C01–C09）

> 结论先行：**内容检测/自动上混属于 S1，P1 明确不实现**（DEC §6、DEC §1、WBS T05）。本节记录既有契约是为了"退役时明确拒绝"，不是让 P1 实现它。**唯一例外 = C09 缺中置内容适配**（用户要求、已实现并接入 host：适配模块由 `rt-host` 自持（task-210），compat 层原型保留为旧 S1/兼容层参考；它不是内容检测/上混，见 C09 与 §0 例外条）。

| ID | 契约点 | 现状（S1）与来源 | P1 处置/要求 | 证据 |
| --- | --- | --- | --- | --- |
| C01 | 运行态文件 | `/run/tspi/input-content.json`，schema `tspi-input-content/v1`；字段 content_mode/detected/override/confidence/active_channels/rms_db/threshold_db/hold_ms/reason/h16_internal_upmix/container_layout/detector/updated_unix | P1 不写该文件、不消费该文件；REST GET 不得假装有检测结果（返回 not_supported/daemon-not-running） | 验：INPUT-MODE.md:10-38；API.md §10；web/src/channelmap.rs:1246-1307 |
| C02 | 覆盖文件 | `/etc/tspi-audio/input-mode.json` schema `tspi-input-mode/v1`；mode∈auto/stereo/multichannel；force-* 别名归一化 | P1 不读不写；PUT 拒绝并保留旧文件不破坏（不得删除用户数据）；如未来恢复，另立版本再评审 | 验：INPUT-MODE.md:40-47；web/src/channelmap.rs:1229-1243,1757-1797 |
| C03 | 优先级 | override > detector > auto；auto 默认直通；detected=stereo 才上混 | 不适用（P1 无上混）；任何"默认上混"行为都被禁止 | 验：INPUT-MODE.md:47,57；web/src/channelmap.rs:1309-1334；DEC §1|
| C04 | 判定依据 | 只用 C/SL/SR/height 的活动判多声道；**LFE 不作判据**（H16 旧固件 2.0 会填 LFE） | 不适用；若未来恢复，LFE 排除规则必须保留 | 验：INPUT-MODE.md:51-53 |
| C05 | 时间参数 | 500ms 采样、hold 2500ms、迟滞 6dB、启动冻结 2.5s；RMS 来自 Camilla `GetCaptureSignalRms` | 不适用；新栈 RMS 命令保留（W18）但不得驱动内容切换 | 验：INPUT-MODE.md:54-56；tspi-upmix:1691-1704 |
| C06 | H16 内部上混标志 | `h16_internal_upmix` 按 detected 推断；true 时板端直通避免双上混；WebUI 提示 | 不适用；P1 不做内容判定，故 H16 内部上混由用户/上游决定，板端只按目标多声道直通映射/显式拒绝 | 验：INPUT-MODE.md:34-35,58-59；DEC §1|
| C07 | P1 接口处置 | 现有 REST GET/PUT + S1 daemon + unit；unit 关键依赖：`tspi-upmix-autodetect.service:4-5` 有 `Wants/After=tspi-camilladsp.service`，`:12` 才是 ExecStart | **RETIRE（P1，状态码见 R47）** | GET 200 not_supported 状态壳、PUT 400 not_supported；UI 同变更隐藏/禁用；**必须禁用/移除 `tspi-upmix-autodetect.service` 并清除其 Wants/After=tspi-camilladsp.service（:4-5），否则 G-no-C 依赖/socket 检查失败**；不得用 fader 切上混路径 | 验：API.md §10、web/src/routes/channelmap.rs:109-127、audio/overlay/etc/systemd/system/tspi-upmix-autodetect.service:4-5,12；DEC §6；WBS §7 |
| C08 | 未知值/缺失文件 | 非规范 mode 返回 400（"mode 必须是 auto/stereo/multichannel"）；state 缺失时 detected=unknown、reason=daemon-not-running；不抛错 | P1 统一按 C07 显式拒绝；任何未知字符串不得猜成 stereo/multichannel | 验：web/src/channelmap.rs:1759-1760,2872-2876；INPUT-MODE.md:94 |
| C09 | 缺中置内容适配（**P1 必须；用户要求项；已实现（host 自持）**；**不是**上混/通用下混） | S1 无自动折叠：4.1.2（无 C 角色）播放 5.1 内容时 C 只被警告/丢弃；新栈 plan 的 A_map 冻结「选择/零填、禁止混音」（core-v1 §1/§11.10），C 内容同样在输入选择层丢失 | **P1 必须**（用户 2026-11-17 裁决：中置暂无音箱，含 C 音轨不得丢中置）：输出布局无 C 角色（中置缺席）且输入内容布局有 C 时，在 **plan 之前的内容适配层**把 C 以 **−3 dB 同时预混进 FL/FR** 并消费 C 源（置零，防重复混）；输出布局含 C 时 passthrough（**不改有中置配置**）；D7 headroom（预测峰值 ≤−6 dBFS、headroom ≥6 dB）不满足时对全部源等量 trim；**不得**把它做成 plan 内 Mixer/上混节点、也不得扩成通用节目 downmix/自动内容上混（C01–C08 仍 RETIRE） | 验：host 自持实现 `rt-stack/crates/rt-host/src/content_map.rs`（`CenterDownmixPlan`，E_CHANNEL_MAP_*；task-210：host 不再依赖/调用 compat；host 用例见 `crates/rt-host/tests/content_map/`，含 −3 dB/有中置/无内容/不重复混/headroom/错误路径与 overlay 读取）、host 运行时接线 task-203（`rt-stack/crates/rt-host/src/main.rs`，证据 `rt-stack/evidence/center-downmix-host/`）、分层收口证据 `rt-stack/evidence/cursor-fix-v1-hostmap/`；compat 层原型 `crates/compat/src/channel_map.rs` + `crates/compat/tests/center_downmix/`（17 用例）保留为旧 S1/兼容层参考；真实 5.1 内容/板端 NOT_RUN |

---

## 7. AoIP 端点与 mode 切换需求（A01–A22）

| ID | 契约点 | 现状与来源 | P1 要求（可判） | 证据 |
| --- | --- | --- | --- | --- |
| A01 | 模式取值（P1 模式面） | 现状 `off/aes67/dante`（web/src/config.rs:198-230）；P1 增加 **`toslink`（stereo_strict 直出）与 `target_multichannel`（目标多声道/file 输入）** 播放模式，与 calibration job 独占互斥（WBS T44）；解析大小写不敏感；未知值 400 | 保持这些播放模式互斥；未知值拒收；AES67/Dante 仍为 AoIP 端点模式；R10/R11 必须接受新值并返回 actual profile/layout/format；内部 calibration 独占态不得占用这些播放值语义 | 验：web/src/config.rs:198-230,849-852、web/src/routes/mode.rs:75-78、WBS T44、DEC §7；推断：新 control 扩展 |
| A02 | CLI 契约 | `tspi-mode dante / aes67 / off [--json] [--dry-run] [--force] [--allow-unlocked-ptp] [--chunksize N] [--target-level N] [--adjust-period N] [--samplerate N]`；`status [--json]`；`--restore` | P1 可由新 control 替代，但 Web/脚本使用的参数与 JSON 输出语义需保留，或显式改调用方；未知参数 exit 2 | 验：tspi-mode:2-14,30-51 |
| A03 | 退出码 | 0 就绪/1 未知/2 参数/3 前置缺失/4 设备/5 PTP 未锁/6 AoIP 流未就绪/7 DSP 启动失败/8 自检失败/9 已回滚/10 另一切换中 | 新 control 必须定义完整映射（现有 Web 映射缺 6，见 web/src/mode.rs:192-205），并让 Web 展示正确；exit 语义与回滚状态一致 | 验：tspi-mode:10-12；web/src/mode.rs:192-205（缺 6） |
| A04 | status schema | `tspi-audio/v1`；顶层 schema/ts/mode/state/layout/outputs/ptp_role/ptp_lock/pid/ptp/input_content/aoip/camilladsp/alsa/last_error | 新栈 status 保留 schema 名与字段名（可 null）；camilladsp 子块改新栈 runtime 摘要或 null；unknown 字段容忍读取（现有 web 方式） | 验：tspi-mode:58-63,454-492；web/src/mode.rs:157-189（ScriptStatus）；parse_script_status 容错在 web/src/mode.rs:1510-1524，属 A05，不属本行 |
| A05 | 状态读取容错 | Web 端 ScriptStatus 用 Option 字段，未知新增字段保留为 Value，未知 schema 仅提示 | 新栈消费方保持"未知字段不崩、schema 前缀校验"；但不得把未知 schema 当成功依据 | 验：web/src/mode.rs:157-189,1200-1202 |
| A06 | REST 切换语义 | PUT /api/mode 202 接受；在途 409；未启用 403；400 未知值 | 保持；切换串行唯一 owner；202 只表示已接受，不代表完成 | 验：API.md §4；web/src/routes/mode.rs:69-86 |
| A07 | 切换编排 | 优先 `tspi-mode --json` 脚本；脚本缺失走内置编排（停 C→停 AoIP→停 PTP→启目标 PTP/等锁→启 AoIP→应用 dante.yml/aes67.yml→启 C→复核；失败回滚） | P1 由新 control 唯一实现；步骤中**不得有 Camilla 启停**；保留目标 PTP/锁/AoIP 就绪/自检/失败回滚顺序语义 | 验：API.md §4；web/src/mode.rs:1113-1238；DEC §6/§7|
| A08 | dante 模式 profile | PTPv1、实际 unit `tspi-statime-inferno.service` + inferno ALSA 插件；96k/24bit、默认 chunksize 128/target 2048；web/src/config.rs 的 statime.service/inferno.service 为 legacy/内置默认名（web/src/config.rs:322-337） | 新栈按实际 overlay unit（`tspi-*`）启动并记录 actual；unit inventory 以 `systemctl list-unit-files 'tspi-*.service'` 为准；若实验/硬件变化须改 profile 并记录 | 验：audio/overlay/etc/systemd/system/tspi-statime-inferno.service、tspi-mode:465、web/src/config.rs:322-337；推断：T71 inventory |
| A09 | aes67 模式 profile | PTPv2；B1 daemon 内建 slave（不启 ptp4l）；实际 unit `tspi-aes67-daemon.service`；48k/24bit；web/src/config.rs 的 aes67-daemon.service 为内置默认名 | 保持 B1 默认；B2/B3 仅显式配置；不得同跑 daemon 与 ptp4l GM；启动/停止以实际 unit 为准 | 验：audio/overlay/etc/systemd/system/tspi-aes67-daemon.service、tspi-mode:467、web/src/config.rs:338-356；RISKS §2 |
| A10 | B1/B2/B3 | B1 默认（外部 GM 才锁）；B2 本机 ptp4l master 但 LKM 拦截/daemon slave-only 冲突；B3 第二网卡实验 | P1 不做模式选择扩展；B1 为交付默认；B2/B3 如启用必须先在 DEC 更新并重评互斥 | 验：RISKS:40-66；aoip REPORT:31-32,96 |
| A11 | 服务单元 | 实际 overlay unit：`tspi-aes67-daemon`/`tspi-statime-inferno`/`tspi-ptp4l-master`/`tspi-phc2sys`/`tspi-ravenna-lkm`/`tspi-camilladsp`/`tspi-upmix-autodetect` 等；web/src/config.rs 默认名与 overlay 名并存 | P1 保留 AoIP/PTP 相关 unit（新 control 启停）；camilladsp unit 必须 disabled/inactive/移除；**upmix-autodetect 必须禁用且清除 `Wants/After=tspi-camilladsp.service`（service:4-5）**；G-no-C 以 systemctl inventory+依赖检查为准；禁止 dependency/socket 自动拉起 C | 验：audio/overlay/etc/systemd/system/*、tspi-upmix-autodetect.service:4-5,12；DEC §7；WBS §7 |
| A12 | AES67 端点参数 | daemon.conf：HTTP 8080、RTSP 8854、RTP 5004、组播 239.1.0.1、ptp_domain 0、ptp_dscp 46、SAP 239.255.255.255、status_file /run/tspi/aes67-status.json、interface 占位、streamer 8 | 新栈渲染配置必须保持这些字段语义；端点选择/rate/format 互斥；status 文件可用于上线验证；不得改端口而 UI/防火墙不同步 | 验：daemon.conf:1-37；aoip REPORT:40 |
| A13 | H16 AES67 流约束 | 每条 multicast flow ≤8 通道；默认组播 239.69.x.x:5004；16 TX；7.1.4 12 路需 2 flow；H16 默认 PTP slave 需外部 GM | 目标多声道 profile 必须按 flow 组合校验通道数；不得假设单 flow 16ch；H16 时钟角色未确认前 P1 AoIP 端到端为 INCOMPLETE | 验：h16-aip-research:196-198,244,270；RISKS:33-35 |
| A14 | Dante/Inferno 端点 | inferno ALSA 插件 capture device "inferno"、S32_LE、96k、16ch；statime PTPv1 virtual clock；device id 由 MAC 派生 +0000；usrvclock 导出 | 新栈必须按稳定 ID/角色 scatter 打开设备；PTPv1 虚拟钟关系保持；inferno2pipe/ALSA 插件可加载不等于链路可用（需真实源） | 验：tspi-mode:159-165,206-222；statime toml:13-17；aoip REPORT:54-61 |
| A15 | PTP/时钟 owner | 每 mode 唯一 PTP 来源：dante=statime PTPv1；aes67=B1 daemon 内置；B2=ptp4l+phc2sys（仅此）；PTP 锁≠USB 同钟；唯一 clock servo | 新栈单一 servo/owner；不得双闭环；phc2sys 仅 B2/B3；discontinuity 新 epoch | 验：DEC §5/§6；aoip REPORT:31-32；web/src/config.rs:240-246 |
| A16 | 失败/回滚语义 | 真机实测：dante rc=5（PTP 20s 未锁，~27s）；aes67 rc=8（缺库，~3s）；off rc=0；失败后 mode=off + state=error + last_error；CueMix diff=0 | 新栈保持"失败不半启、状态可解释、last_error 可查、回滚到 off；用户听音路由不变"；耗时仅记录不承诺 | 验：aoip REPORT:79-88 |
| A17 | 端点/格式/速率互斥 | 新 host 必须完成端点选择/通道/format/rate/PTP-state 互斥（DEC §6） | 同一时刻单一 capture/playback owner；模式切换期间不双开设备；format/rate 变更需重准入；actual 参数必须返回 | 验：DEC §6；WBS §4（T17） |
| A18 | 稳定 ID/role scatter | 输入/输出稳定 ID 与 role scatter 显式；不得假定 card4 永远不变 | 新栈必须解析实际设备/通道并绑定 hash；设备缺失时拒启而非错位输出 | 验：DEC §6；WBS §3；API.md §10|
| A19 | 真实证据要求（file 输入与真实 AoIP 拆开） | P1 G-play 允许真实 Toslink/AoIP 或 **预渲染 4.1.2 file 输入驱动 MOTU** 作为签收路径（DEC §7、WBS T74）；文件/loopback 不能替代真实 AoIP 端点，但 file 路径本身必须实测 actual playback/布局/听音 | file 路径：hash/布局/actual playback/听音证据齐全即可签 G-play-file 子项；真实 AoIP 子项须 H16/第二端点+PTP 锁，缺硬件记该子项 NOT_VERIFIED/INCOMPLETE，不阻塞 file 路径；不得用本机 loopback 冒充真实 AoIP | 验：DEC §7、WBS T74、WBS 最终验收补充；推断：T74 执行 |
| A20 | 已知阻塞 | AES67 daemon 缺 5 库；statime panic "could not get clock identity"；ptp4l 模板行尾注释+多余 [eth0]；inferno 无时钟 | P1 排期必须显式跟踪/修复；未修复则 AoIP 门 INCOMPLETE；不得靠绕开 daemon 假装通过 | 验：aoip REPORT:12-19,34-61；docs/CURSOR-TASK-AOIP-BLOCKERS.md:16-20；PROJECT-STATUS:27 |
| A21 | 开机自动恢复 | D4 默认 false：上电音频 off、不自动恢复；`tspi-boot-mode.service` 默认 disabled | 新栈保持默认不自动恢复；如要自动恢复须用户显式配置并记录 | 验：web/src/config.rs:315-316；RISKS:81-83,100-108 |
| A22 | MOTU 控制面不受影响 | CueMix 控制通道 UDP 1280 信标+WS 1280；切换前后用户直通（Optical1/2→Main1）diff=0；final insert 可关闭直通 | 模式切换/校准 insert 必须保留快照/恢复/留证流程；不得改动用户 Toslink 直通；MOTU 离线写 503、读 200 语义保持 | 验：PROTOCOL-REPORT:6,14；aoip REPORT:77-88；API.md §9.1|

---

## 8. 保护/限幅需求清单（P01–P14）

| ID | 保护项 | 现状依据 | P1 要求（可判） | 证据/状态 |
| --- | --- | --- | --- | --- |
| P01 | 输出格式转换语义 | 新 host 必须维护 S24_3LE/S32_LE interleaved↔planar、端序、饱和 clip、nonfinite、dither | 每种格式给出明确转换规则；溢出**饱和不缠绕**；NaN/Inf 输入不得直接送 DAC（sanitize+mute+计数）；dither 位置/幅度策略显式；边界向量测试非零 | 验：DEC §6；WBS T22；推断：具体阈值 T22 冻结 |
| P02 | 故障计数/直方图 | 现有 C 侧只有 ClippedSamples/日志线索；新栈要求固定计数 | nonfinite/clip/xrun/deadline miss/queue under-overflow/worker 迟到均有固定计数与有界 histogram；单调/epoch 重置规则明确；非 RT 导出可读 | 验：DEC §5；API.md §6；推断：字段新栈定义 |
| P03 | per-output limiter/max gain | v2.1 草案 `roles[].limiter{threshold_db,release_ms}`、`max_gain_db`；MIMO 原型每输出 limiter≈0.89（-1dBFS） | 明确限幅器插入点（角色求和后、scatter 前）；threshold/release 生效；max_gain_db 硬帽；限幅不得被 bypass/identity 回退绕过；参数未定项见 Q07 | 验：API.md §10；MIMO-ROADMAP:124-125；MIMO-ENGINE-EXPLAINER:147；audio/tools/mimo-engine/README.md:12 |
| P04 | 增益范围与裕量 | channelmap gain -60..+12；校准 trim -12..+6；MIMO 设计增益帽 +6、limiter -1dBFS、设计裕量≥6dB | 所有增益（role/PEQ/boost/trim）叠加后必须可证明不越界；PEQ 正提升总量有界；设计/应用前预测峰值；超限拒收或降级并记录 | 验：API.md §10；CONTRACT.md:84,130；MIMO-COEFF-PLAN:301 |
| P05 | 延迟/极性/相位保护 | delay 0..200ms 整数样本、min=0；polarity XOR；pair 合并相位/抵消门在 ACCEPTANCE | 延迟/极性只经单一通道字段应用；禁止双扣/双乘；apply 前校验越界；应用后按 T3/T4 复测（不达标不签） | 验：API.md §10；CONTRACT.md:111-124；WBS 最终验收补充|
| P06 | 回退保护图 | 过载/故障切 G identity 仍保 P 最终保护；无法安全输出则预分配 mute；bypass 不绕保护；fallback graph 预分配且准入 | 双图预分配/准入；未 ack 不可复用；RT 不分配；保护路径本身有测试向量（identity+非零 P） | 验：DEC §5/§7；WBS §6；推断：由 T20/T21 实现 |
| P07 | 音量/静音 | Camilla volume -150..+50 且返回实际值；MOTU main_trim 0..=100（-n dB）；final insert 一键关直通 | 新栈 volume/mute 量程与实际值语义；音量变化做 ramp（防爆音）；mute 与保护静音可区分；MOTU trim/final insert 语义保持 | 验：API.md §6/§9.1；web/src/camilla.rs:590-608；推断：ramp 参数 T42 |
| P08 | 校准扫频安全 | 用户授权可闻安全电平/原路由快照；sweep 声明的电平/ramp/保护/clip 停止/cancel；设备独占 | 扫频前 preflight（电平/路由/设备/时钟）；异常立即停并恢复 last-good/mute；cancel 可中断并回安全态；不得后台抢声卡 | 验：DEC §2；WBS T61、WBS T60 |
| P09 | xrun/超时/半块保护 | 固定 SPSC；四 worker 上界 spin/cutoff；迟到整块无效；playback 不读半成品 | 超时/under/overflow 明确记录并走 identity/mute；不输出半块；deadline miss 计数与降级策略可测 | 验：DEC §5；WBS §6|
| P10 | 启动/断流/恢复 | discontinuity 新 epoch，不拼旧历史；冷启/切换/恢复计数公开 | 设备失锁/格式变化/切换均新 epoch；history 复用需证明；恢复流程可观测、可回滚 | 验：DEC §5；WBS T30 |
| P11 | 切换期操作安全 | 真机：切换前后 CueMix 快照 diff=0，用户 Toslink 直通未动；失败回滚到 off | 任何模式/格式/校准 apply 前必须快照保护用户可闻路由；失败恢复并留证；不得静默改动用户直通 | 验：aoip REPORT:77-88；API.md §9.1|
| P12 | 校准应用/回滚保护 | 校准只读原文件；应用前 validate；失败回滚（现有 profile apply 同）；unknown type 拒收；curve 不双乘 | apply 幂等/可回滚；stale/hash 不匹配拒收；last-good 可用；禁止把旧 C 当回滚 | 验：CONTRACT.md:141-163,220-222；WBS §5；DEC §7|
| P13 | 限幅/电平验收证据 | MIMO 测试门：limiter 触发 <0.1%、最大样本 ≤ -1 dBFS；每输出 | P1 保护验收必须给出真实节目/测试信号下每输出峰值与 limiter 统计；不达标不签 | 验：MIMO-ROADMAP:400；推断：P1 复用该门 |
| P14 | 未决保护参数 | attack/lookahead/true-peak、limiter threshold 每输出、dither、clip 告警阈值均未定 | 由 T02/T22 + 用户拍板后冻结；本文件只锁"必须有且不可绕过"，不虚构数值 | 推断：Q07 |

---

## 9. 未知项拒收策略（X01–X22）

> 目标：任何"命令/字段/类型/版本/角色/参数"未知时，**可判地拒绝**，不静默、不降级、不猜测、不回 C。

| ID | 面 | 场景 | 现状行为与来源 | P1 强制策略（可判） | 状态 |
| --- | --- | --- | --- | --- | --- |
| X01 | REST | 未知路径/方法 | 未知非 `/api` 路径由静态 handler 返回 404 纯文本；`/api/*` 未知路径实际行为未运行验证（axum nest fallback） | 所有 `/api/*` 未知路径返回 JSON 404 `{"error":{"code":"not_found"}}`；已知路径错误方法返回 405 JSON；**禁止**把未知 API 落到静态页 | 验：web/src/assets.rs:32-48、web/src/routes/mod.rs:125；未测：/api fallback 实际行为 |
| X02 | REST | 请求体未知字段 | 现有 handler 全部 serde 默认（无 deny_unknown_fields，grep 0 命中） | **冻结策略见 §2.4**：ADD 新端点严格 400 `unknown_field`；ADAPT 端点按各 R 行冻结 request struct；KEEP/迁移期白名单（R19 `{db}`、R28 `channel`、auth/session/login、motu confirm/trim 等）允许额外字段但须在 §2.4 记录；不再保留“待补宽容清单”状态；与 Q03 结论一致 | 验：web/src 无 deny_unknown_fields；推断：serde 默认；见 §2.4 |
| X03 | REST | 缺失/类型错误 JSON | 无自定义 rejection 处理器（grep 0 命中）；axum 0.8 默认提取错误非统一 envelope | 统一 JSON 错误 envelope；**syntax 与 data 错误均映射 400**（`bad_request`/`invalid_json`/`invalid_field`）；不使用 422（不在 API.md §1 状态集合）；由 T42 实现自定义 rejection | 验：web/Cargo.toml:10、web/src/error.rs:92-101、web/src 无 rejection；推断：axum 默认 |
| X04 | REST | 未知 mode | 400 bad_request，文案列出 off/aes67/dante | 保持 400；文案含允许值；不默认回 off 假装成功 | 验：web/src/config.rs:222-230；web/src/routes/mode.rs:75-78 |
| X05 | YAML | schema/version 不匹配 | channelmap rc=2；presets 加载即拒；calibration schema error | 所有文件入口先校 schema/version；不匹配整份拒收；不"尽量解析" | 验：tspi-channelmap:106-107,164-167；CONTRACT.md:185,196 |
| X06 | YAML | channelmap 未知字段 | `check_keys` 报 error（"未知字段（v2 严格校验）"） | 保持整份拒收；错误带 JSON path；不得仅警告后应用 | 验：tspi-channelmap:78-85；API.md §10|
| X07 | YAML | 校准未知 filter type | 校验器/契约：未知 type=error，整份拒收；v1 只允许 peaking | 新栈 T13 同样整份拒收；不得降级 peaking；不得丢 type | 验：CONTRACT.md:137,251；validator:225-232；已执行内存复核（§13.2） |
| X08 | YAML | 校准未知顶层/通道字段 | 契约=warning（保留扩展性），不参与应用 | 可 warning 但必须上报；未知语义字段不得参与应用；不得因 warning 变 error 误拒扩展 | 验：CONTRACT.md:196；validator:74,169；已执行复核 |
| X09 | YAML | 单位不明/NaN/缺幅度/混合 phase/无参考/超预算（target） | 草案要求拒收 | 一律拒收并给字段；缺 phase 选明确 family，不猜零相位；频点必须正/递增/唯一/≤fs/2 | 验：DEC §3；WBS §6|
| X10 | WS | 未知命令 | 现有客户端未覆盖；未回复→客户端超时 | 回复同键错误（如 `{"<cmd>":{"result":"UnknownCommand"}}` 或等价错误对象），连接可继续；不得静默/挂起 | 推断：web/src/camilla.rs:259-269；tspi-upmix:1666-1678；Q08 |
| X11 | WS | 非法 JSON/帧类型/空命令 | 现有客户端不发；行为未测 | 错误回复或带原因关闭；不得 panic；单帧大小上限；错误信息不泄露路径 | 推断：新栈契约 |
| X12 | WS | 值越界/类型错 | SetVolume 客户端侧 -150..+50；其余未验证 | 服务端同样校验并显式错误；SetVolume/SetFaderVolume 越界拒绝；返回 actual 值 | 验：web/src/camilla.rs:590-593；推断：服务端 |
| X13 | WS | 未知 fader id | S1 只用 1/2 | 未分配 fader id 明确错误；不得写越界内存/静默忽略；fader 不得绕过保护 | 验：tspi-upmix:1707-1717；推断：新栈 |
| X14 | REST | 不支持的 API 版本 | 无版本字段 | **可选**请求头 `X-TSPI-API-Version`：缺省=v1；仅显式给出不兼容版本时 400 `unsupported_api_version`；响应头回 `X-TSPI-API-Version: 1`；禁止设为必填（会误杀全部现有客户端） | 推断：R50 |
| X15 | REST | 退役端点被调用 | 现状不存在“退役”概念 | **一般退役端点：410 `{"error":{"code":"retired","replacement":"..."}}`**；**唯一例外：R47 input-mode 按“GET 200 not_supported 状态壳 / PUT 400 not_supported”**（理由：UI 轮询兼容）；两者都不允许纯 404 纯文本或假成功；UI 同变更更新；K13/K16 按此判 | 推断：§2.1 RETIRE + R47 |
| X16 | CLI | 未知参数/子命令 | tspi-mode 未知参数 exit 2；channelmap argparse exit 2 | 新 CLI 保持 exit 2 + stderr 说明；不得忽略未知参数继续执行 | 验：tspi-mode:48；tspi-channelmap:6-7,25 |
| X17 | 状态 | 未知 schema | Web 只提示"未知 schema"仍展示；不当作成功依据 | 消费方可容错读取，但不得以未知 schema 判定 ok；新 control 输出 schema 必须受版本校验 | 验：web/src/mode.rs:1200-1202,1510-1524 |
| X18 | 校准 | stale/hash/role 不匹配 | 既有契约要求 apply 前校验；T13 新增 IR stale | 明确拒收并给 stale 原因（input/fs/role/P/hash 变化）；不得用旧系数应用 | 验：WBS T13/§5；DEC §2|
| X19 | channelmap | 未知角色/输入源/设备/通道越界 | 现有 rc=3 / 400，多处校验 | 全部越界/未知一律拒收；错误带角色与通道；不得截断/复用错位 | 验：tspi-channelmap:385,457-502；API.md §10|
| X20 | 横切 | 静默降级/假成功/回 C | 现有实现尚可（channelmap 严格），但部分入口宽容 | 全局禁止：未知字段丢弃、未知 type 降级、C 兜底、错误当 OK、旧文件冒充真值；**已声明 `x_`/扩展字段 warning 不属“丢弃”**；发现即缺陷；upmix unit 的 C 依赖（Wants/After）必须清除 | 验：CONTRACT.md:137；tspi-channelmap:409-411；DEC §7；tspi-upmix-autodetect.service:4-5 |
| X21 | 横切 | 错误 envelope/认证 | 统一 `{"error":{"code","message"}}`；401/403/429 语义 | 新端点沿用；错误不泄露 token/密钥/内部路径；CSRF/session 不弱化 | 验：API.md §1；web/src/error.rs:44-101；web/src/routes/mod.rs:132-151 |
| X22 | 横切 | 大小/内容限制 | 配置 >2MiB 400；FIR >16MiB 400；snapshot raw >512KiB 省略正文 | 保持上限并文档化；超限 400/413 明确；不得截断后假装成功 | 验：web/src/camilla.rs:128；API.md §9.1/§10|

---

## 10. 人工检查表（K01–K60，逐条可判）

> 用法：每条给出可观察判据。**当前状态**列：D=本文档即可判（评审项）；I=实现后判（未来门）；R=需真实硬件/实测（T74/T72）。P1 最终不允许只签 D。

| ID | 检查项 | 通过判据 | 对应矩阵行/状态 |
| --- | --- | --- | --- |
| K01 | 现有 REST 面全覆盖 | 逐条核对 `web/src/routes/mod.rs:29-123` 的 60 条 route 条目/67 个方法操作，全部能映射到 R01-R51 之一，无"无主"端点 | R01-R51/D |
| K02 | 错误 envelope 一致 | 每个 `/api` 错误响应均为 `{"error":{"code","message"}}`；未知路径/方法也如此 | R50,X01,X03,X21/I |
| K03 | 认证/会话/CSRF | 未认证 401；会话 Cookie+CSRF；header token 免 CSRF；清空会话后 401；setup 仅首次 | R02-R06/I |
| K04 | overview 字段稳定 | UI 读到的 mock/system/mode/camilla/ptp/alsa/services/service_notes/power 键均存在且类型不变（camilla 键语义可换但字段映射有记录） | R07/D+I |
| K05 | mode GET/PUT 语义 | GET 字段齐全；PUT 202；在途 409；未启用 403；未知值 400；**支持 toslink/target_multichannel 并回 actual profile/layout/format** | R10,R11,A01,A06/I |
| K06 | script-status 失败不 500 | 脚本缺失/不可用时返回 available:false+message，HTTP 200 | R12/I |
| K07 | audio/format 边界 | 至少一个字段；不在允许集合 400；actual 参数与请求一致；chunksize=32 有风险提示 | R13,R14/I |
| K08 | Camilla 面处置明确 | 每个 `/api/camilladsp/*` 在表内有唯一 KEEP/ADAPT/RETIRE 决定；P1 无 C 进程；R15/R17/R18/R19=ADAPT、R16=ADAPT；若后续 RETIRE 有 410+UI 同步 | R15-R19,A11/I+R |
| K09 | PTP 面不假锁 | B1 无接口时显示 unknown/未锁；B2 下 pmc 状态一致；start/stop 不误启 | R20-R22,A15/I |
| K10 | AES67/Inferno 面 | status/logs/config/start/stop/restart 语义与互斥；日志上限 1..2000；失败不假成功 | R23-R26,A08-A10/I |
| K11 | ALSA/MOTU 面 | 卡/PCM 实测通道来自 /proc；MOTU 读离线 200 online:false、写离线 503、confirm 缺失 400；写后读回/回滚 | R27-R33,P07,P11/I+R |
| K12 | channelmap 全操作 | 18 个方法操作逐条可用；validate 由权威器执行（CLI 不可用=503 不本地放行）；apply/rollback 有 last-good | R34-R46,Y01-Y11/I |
| K13 | input-mode 明确退役 | GET 200 `{status:"not_supported"}`；PUT 400 `not_supported`；UI 隐藏；无内容 daemon/fader 切换；upmix unit 依赖清除 | R47,C07,X15/I |
| K14 | 校准 job 不变量 | 状态机全集；设备独占；cancel 安全；apply actual ack；retest/rollback 可查；失败 last-good/mute | R48,P08,P12/I |
| K15 | 新 runtime API actual version | status/准备/应用/音量响应均带 actual version/hash；prepare≠commit；冲突 409 | R49,R50/I |
| K16 | 静态资源与 404 | 同源无外部 CDN；CSP 生效；未知 `/api` 返回 JSON 404；静态未知 404；退役端点按 X15（R47 例外） | R51,X01,X15/I |
| K17 | WS 帧/请求/回复形态 | 文本 JSON；字符串或单键对象；回复同键 result/value；非法帧拒绝 | W01-W03/I |
| K18 | WS 读命令齐全 | 12 条 live 命令 + GetConfig 全部可回且类型正确；不要 GetXruns | W05-W15,W22/I |
| K19 | WS SetConfigJson 实际 ack | 坏图拒绝且不生效；好图生效后才 Ok；返回新版本；无 C 重启 | W16/I |
| K20 | WS 音量/静音 | 范围/越界错误/actual 读回一致 | W09,W17/I |
| K21 | WS RMS/fader | RMS 数组长度=采集通道、dBFS 有限；fader 已知 id 可写、未知 id 错误、不绕保护 | W18,W19/I |
| K22 | WS 未知项拒绝 | 未知命令/非法 JSON/越界值均给同键错误且连接可继续，不挂起 | W20,W21,X10-X13/I |
| K23 | WS 部署安全 | 仅 loopback 或鉴权；端口/URL 文档化；不新增未认证 LAN 面 | W01-W22 横切/I |
| K24 | channelmap 严格 schema | 未知顶层/块/角色字段全部 rc=2/400；schema/version 精确；FIR 无效支撑按 R45 准入拒绝 | Y01,Y02,Y07,Y22,X05,X06/I |
| K25 | channelmap 输入/输出 | input/output 键语义；auto 通道与实测一致；未知布局拒收 | Y03,Y04,I |
| K26 | 角色边界 | gain/delay/polarity/HP/LP/PEQ 上限逐项越界拒收；fan-out 支持 | Y05,Y23/I |
| K27 | PEQ 类型/斜率 | type 白名单；shelf q=S∈[0.1,1.0]；渲染 slope=12*S；不得丢 type | Y06,Y23/I |
| K28 | FIR 引用 | 路径/fs/taps 校验；fs≠模式拒绝；>4096 实验/>8192 强警告；不截断 | Y07,I |
| K29 | remap/bass management | 未知 preset/角色拒收；sources 与 remap 互斥；bass 规则保持 | Y08,Y09,I |
| K30 | layout-presets 与 4.1.2 | 解析而非硬编码；4.1.2 输出=FL0 FR1 SL2 SR3 HFL4 HFR5 SUB7；ch6 空；drift 检查保留 | Y10,Y11,A18/D+I |
| K31 | 校准 v1/v1.1 接受 | 两个版本合法文件通过；v1.1 shelf 保留 type；未知 type 拒收整份 | Y12,Y13,Y14,X07/I |
| K32 | 校准映射语义 | delay/极性/PEQ/gain 按 XOR/相加/追加；role 不存在拒收；curve 不双乘 | Y15,P05,P12/I |
| K33 | 校准生命周期/stale | 历史只增不改/原子写/current 选择；hash/version/role/stale 字段；stale 拒 apply | Y16,Y24,X18/I |
| K34 | target 导入严格 | 按 T04 §6：单位/NaN/缺幅度/混合 phase/无参考/超预算/频点非法逐项拒收；target_delay_samples 仅 delay_removed 必填；缺 phase 选 family；**拒收码全集见 T04 §6.9（T04 v1.2-draft 为 28 码；以 T04 当前版本为准，不写死码数/行号）** | Y17,X09,T04 §6/I |
| K35 | 内容态字段（S1 资料） | 字段名/类型与 INPUT-MODE 一致；P1 不消费 | C01,C02/D |
| K36 | 内容判定规则（S1） | LFE 不作判据；窗口/迟滞/冻结值记录 | C03-C05/D |
| K37 | H16 内部上混提示 | h16_internal_upmix 语义与双上混禁止记录 | C06/D |
| K38 | 内容检测 P1 处置 | 无 daemon、无 fader 切换、REST 显式拒绝、UI 隐藏 | C07,C08,R47/I |
| K39 | 模式值/CLI/退出码 | off/aes67/dante/toslink/target_multichannel 互斥；未知参数 exit 2；tspi-mode exit 0..10 含 1/6 的映射完整 | A01-A03/D+I |
| K40 | mode status schema | tspi-audio/v1 顶层/嵌套字段存在；unknown 字段消费方不崩 | A04,A05,I |
| K41 | 切换状态机 | 202 接受、串行、步骤/回滚/last_error 可见；无 C 步骤 | A06,A07,A16/I |
| K42 | Dante profile | PTPv1/statime/inferno/96k/24bit/稳定 ID | A08,A14,I+R |
| K43 | AES67 profile | B1 daemon 内建 slave、48k/24bit、端口/status 文件 | A09,A12,I+R |
| K44 | PTP 归属与互斥 | 单 servo/单 owner；B2 才 phc2sys；PTP 锁≠USB 同钟 | A10,A15,I+R |
| K45 | 端点/格式/速率互斥 | 同一时刻单 capture/playback；format/rate 变更重准入；actual 返回 | A17,DEC §6/I+R |
| K46 | H16 流约束 | 每 flow ≤8ch；7.1.4 需 2 flow；组播/端口与模板一致 | A13/D+I |
| K47 | 真实证据门 | file 输入路径可签 G-play-file（hash/布局/actual playback/听音）；真实 AoIP 需 H16/第二端点+PTP 锁，缺硬件该子项 NOT_VERIFIED/INCOMPLETE | A19,A20/R |
| K48 | 开机/用户路由安全 | 默认不自动恢复；切换前后 CueMix 快照 diff=0；用户直通不动 | A21,A22,P11/I+R |
| K49 | 格式转换保护 | 饱和/clip/nonfinite/dither 规则有测试向量；NaN 不进 DAC | P01,P02/I |
| K50 | 限幅/增益/裕量 | limiter threshold/release/max_gain 生效；所有 boost 叠加可证不越界；limiter 不可绕过 | P03,P04,P13/I+R |
| K51 | 延迟/极性/相位 | 越界拒收；单一应用路径；apply 后复测门 | P05,P08/I+R |
| K52 | 回退保护图 | 双图预分配/准入；identity 保 P；unsafe 预分配 mute；无 RT 分配 | P06,P09/I |
| K53 | 音量/静音 | -150..+50；mute 独立；ramp 无爆音；actual 回读 | P07,I |
| K54 | 超时/断流/启动 | 新 epoch、无半块、计数可读、恢复可回滚 | P09,P10,I |
| K55 | 操作安全快照 | 模式/格式/校准 apply 前快照与失败恢复留证 | P11,I+R |
| K56 | 校准 apply/rollback | 幂等、可回滚、stale 拒收、last-good 非 C | P12,I |
| K57 | REST 拒收策略 | 未知路径/字段/版本按 X01,X02,X14；退役按 X15（R47 例外）；envelope 一致；字段白名单见 §2.4 | X01-X03,X14,X15,X21,§2.4/I |
| K58 | YAML/CLI 拒收策略 | schema/字段/type/数值/未知参数逐项按 X04-X09,X16-X20 拒绝 | X04-X09,X16-X20/I |
| K59 | WS 拒收策略 | 未知命令/非法帧/越界/fader id 按 X10-X13 拒绝且不挂起 | X10-X13/I |
| K60 | 证据诚实性 | 无"0 tests PASS"、无只编译 PASS、无预测冒充实测、无 S0-S3 代替 P1；缺证据=INCOMPLETE | WBS §6/§8;DEC §0/§7/I |

---

## 11. 与 WBS 任务的追溯

| WBS 任务 | 本文件输入 | 说明 |
| --- | --- | --- |
| T02 api-designer（core-v1） | R48-R50 的 plan/commit/actual-version 语义、W01-W22 的 WS 边界 | **T02 只承接 plan/ack/actual-version 语义**（PlanCommitted 后才可返回 applied、graph_id 用于 REST/WS actual ack）；**REST/WS 外形由本文件/T24/T42 定义**，不归 T02 |
| T13 calibration-adapter | Y12-Y24、X05-X09、X18 |  v1/v1.1 只读兼容、IR 草案 version/hash/role/stale、unknown 拒收 |
| T24 control-author（拟建 crates/compat/src/ws.rs） | W01-W22、R15-R19 | WS 必需子集、prepare/commit 实际 ack、RMS/rate/volume/mute/fader、未知命令拒收；**R15-R19 在 §3.7 为 ADAPT-DEFERRED（P1 不注册路由）** |
| T25 channel-map-author | Y01-Y11、R34-R46、C07 | 可变 N/O 扩展点；stereo strict；禁止 runtime mask 破坏 G 预测；**R34-R46 在 §3.7 为 ADAPT-DEFERRED（P1 不注册路由）** |
| T42 rest-author | R01-R51（尤其 R48-R50）、X01-X03；落地状态见 §3.7 | 自有 REST 鉴权/错误/冲突/actual version；校准 job 入口；**F-21 矩阵↔路由一致性测试**（tests/T42/matrix_routes.rs） |
| T44 mode-author | A01-A22、R10-R12 | 模式/端点/独占/PTP owner/回滚 |
| T57 role-correction | Y10-Y11、A18 | 唯一 overlay 解析与 hash；4.1.2 映射 |
| T60 cal-job-author | R48、P08、P12、Y16 | job 状态/互斥/progress/cancel/checkpoint/恢复 |
| T70 web-author | R01-R51、W01-W22、K01-K16 | 真实页面日常操作；非占位按钮/只 API；新 UI 只消费 §3.7 已落地行 |
| T30（P2-performance，后续） | P01-P14 的 R 类证据、A19 file/AoIP 子项 | P2 门（WBS T30：1h 0xrun/64 独立最终窗），**不属 P1 签收**；缺证据=INCOMPLETE（P2） |
| T74 real-playback-web | 全部 R/A/P 的 R 类检查项（§3.7 DEFERRED 行不在 P1 验收内） | 真实输入源与 Web 实测；缺真实源=INCOMPLETE |
| T71/T72 retirement | R15-R19（§3.7 已 DEFERRED；旧 web 启停已删）、A11、C07 | 无 C 进程/units；内容检测与 upmix 相关 unit 处置见 Q04 |

---

## 12. 未决问题（需 Lead/用户裁决；不阻塞本文件评审）

| ID | 问题 | 影响 | 建议 |
| --- | --- | --- | --- |
| Q01 | P1 Web 入口是"保留现有 khaen-audio-web 改后端"还是"T70 重写新 Web"？ | 决定 R15-R19 是 ADAPT 还是 RETIRE；决定 auth/CSRF/静态资源是否保留 | 建议保留 auth/system/channelmap/motu/alsa 面，runtime/校准页面按 T70 新建；无论哪种，R 表用途不变。**F-21 落地：Phase 1 镜像不安装旧 web，T70 只消费已落地行，未落地旧路径见 §3.7（DEFERRED）** |
| Q02 | WS 兼容子集消费者/是否保留 Camilla shim | 部分已由 WBS T24 回答：保留 RMS/fader 等必需子集；T70 重写 UI 不改变该子集 | **已决**：按 T24 保留子集，不实现完整 Camilla 协议（不再等待裁决） |
| Q03 | 过渡期 REST 未知 JSON 字段策略 | 已由 §2.4 冻结：ADD 严格/ADAPT 冻结 struct/KEEP 白名单；影响 X02/K24/K57 | **已决**：策略见 §2.4；T70 完成 UI 审计后收紧白名单 |
| Q04 | 内容检测/上混 unit 退役范围 | 已由 DEC §6 决定必须禁用/移除内容自动上混 daemon；G-no-C 还要求清除 unit 依赖 | **转执行项**：C07/A11/X20 已加入“禁用 tspi-upmix-autodetect + 清除 Wants/After=tspi-camilladsp.service（service:4-5）”可判条目 |
| Q05 | PTP/AoIP 服务归属 | 大部分已由 DEC §6 回答：保留现有服务、new host 做选择/互斥/唯一 owner | 已决：systemd 为执行层、新 control 唯一编排 owner；T44 只需冻结调用边界 |
| Q06 | 校准 job REST 的具体路径/状态字段/schema 由谁冻结？ | T42/T60/T70 对接 | 建议 T02 给接口原则、T42 冻结 REST 外形、T60 冻结 job 状态与 artifact schema，联合评审 |
| Q07 | 保护限幅参数：attack/lookahead/true-peak/每输出 threshold/release/dither 取值？ | P03/P14 与声学安全 | 建议按 MIMO 既有门（-1 dBFS、50ms）起步，补 lookahead/true-peak 决策后再冻结 |
| Q08 | WS 未知命令的精确回复结构/错误码字符串/协议版本字段？ | T24 测试与客户端健壮性 | 建议与 T02 一起冻结 envelope：`{"<cmd>":{"result":{"Error":"unknown_command:...>"}}` 或等价，并写进 T24 测试 |
| Q09 | audio/format 允许范围 | **已决**：runtime 仅 64/128 chunksize（T02 core-v1 Q-11），其它值 REST 返回 400/unsupported；UI 用后端 supported 列表动态渲染，不静默夹取；采样率生产 48k/96k | 已决（本文件）；T42/T70 实现 |
| Q10 | calibration v1.1 peaking q>10 在 channelmap 只允许 ≤10 的边界如何落到新栈？ | T13 映射与窄模态实现 | 建议新栈 graph 用独立 IIR/FIR 片段承载（等价 CONTRACT §4.2），channelmap 边界不变；需 T13 明确 |
| Q11 | 新 control/WS 的鉴权：复用现有 session/CSRF/token，还是 WS 仅 loopback？ | 安全边界与 UI 改造量 | 建议 REST 复用现有 auth；WS 默认 loopback + 可选 token；不新开未认证 LAN 端口 |
| Q12 | 真实硬件阻塞（H16、第二 AoIP 端点、UMIK）时间窗 | A19/A20 与 P1 最终门 | 建议 Lead 在 P1 排期显式挂依赖；未到货则相关门 INCOMPLETE，不降级签收 |
| Q13 | RETIRE/R16 状态码与唯一处置 | 已由 §2.1/X15 冻结：一般 410 retired；R47 GET 200/PUT 400 not_supported；**R16=ADAPT-DEFERRED**（F-21：P1 无 C 进程、旧 web 启停路由已删，见 §3.7） | 已决（本文件） |
| Q14 | 8192/16384 extended 准入 vs UI 警告 | 已由 R45/Y07 冻结：>4096 需 T56 profile 准入、8192/16384 extended、>16384 运行期拒收；UI 标签非准入 | 已决（本文件）；T56 实现准入字段 |
| Q15 | P1 模式面 toslink/目标多声道 | 已由 A01/R10/R11/K05/K39 补入；与 calibration job 独占互斥 | 已决（本文件）；T44 实现 |
| Q16 | target 导入字段与 T04 对齐 | 已由 Y17/K34 改为引用 T04 §6 全字段/拒收码 | 已决（本文件）；T11 按 T04 实现 |
| Q17 | unit inventory 命名（web/src/config.rs legacy vs overlay tspi-*） | 已由 A08/A09/A11 冻结：实际以 overlay tspi-* + systemctl 为准，web/src/config.rs 值标 legacy | 已决；T71 提供 inventory 证据 |
| Q18 | api_version 兼容策略 | 已由 X14 冻结：可选请求头、缺省 v1、仅显式不兼容版本 400；不做必填 | 已决（本文件）；T42 实现 |

---

## 13. Manifest

### 13.1 输入快照（引用策略：路径+§章节/任务 ID；hash 仅快照）

> 快照时间 **2026-11-17 13:3x（Asia/Shanghai，task-145 冻结刷新）**；上游 T02/T04 在快照后仍可能被作者继续修改（标记为 volatile，复审前按 §13.6 复算）。DEC/WBS/API.md 引用一律用「文件路径 + §章节 / WBS 任务 ID」，不写死行号与 hash；下表 hash 仅用于复现冻结快照；本轮只改引用不改变任何矩阵语义。

> **task-175 刷新（2026-11-17 19:0x，T02 v1.1.3 errata E3/E4 后）**：实测刷新 4 行——T02 core-v1（183,197 / `b6420ce922ae663c58b58dd98d6639c2d6ad27bde617384c91d9a51f7e05d04d`）、`web/docs/API.md`（26,790 / `4255f1ad4a4f0e2c1d4b2966aa7ad7cfa26373b904cb13f6955d161e3b721954`）、`BASELINE-INVENTORY.md`（16,427 / `e98c0e8fd25570bcb705f5fa60b03242c9969d12331c7814aae95f6b5908cffa`）、`web/src/routes/camilladsp.rs`（6,295 / `118efe0efa2e07cfe89e305773ede872cb86efb26e5b41b2bf9f9fec50bcfe5e`）；其余行经 `check_snapshot_hashes.py` 复核未变。只改引用，不改任何矩阵语义。

| 输入 | 字节 | sha256（快照） | 用途/备注 |
| --- | ---: | --- | --- |
| scratch/lead/planner/TSPI-RT-STACK-WBS.md | 35728 | `ffe5cde9e27470f0c2db9c4242383478422f553defba9370c8d268d0643204b9` | 任务/DoD/运行边界（引 WBS Txx/§n） |
| scratch/lead/planner/DSP-HOST-DECISION.md | 27951 | `ab1056477d8e1c01dbe61df6f2d15c4bb0cca6b05f9177ef51559df3e2d25ded` | 主决策/门/RT 规范（引 DEC §n） |
| web/docs/API.md（task-175 刷新） | 26790 | `4255f1ad4a4f0e2c1d4b2966aa7ad7cfa26373b904cb13f6955d161e3b721954` | REST 语义快照（引 API.md §n） |
| docs/MIMO-COEFF-PLAN.md | 45259 | `fc832832487e73182787ad19e4fb3639edb3355584a33a5f9a4d707f539c3f30` | 测量相位口径 |
| rt-stack/spec/fir-toolbox.md（T04 v1.2-draft，volatile；task-145 刷新） | 97033 | `86fb97fdd7dbbffda8207d49de0c4150e1b131fe470b7939a32b6900b777b6a5` | target schema/家族/28 码表（Y17 引 T04 §6；快照后可能继续变） |
| rt-stack/spec/core-v1.md（T02，volatile；task-145/task-175 刷新） | 183197 | `b6420ce922ae663c58b58dd98d6639c2d6ad27bde617384c91d9a51f7e05d04d` | Q-11 runtime 64/128、plan/ack/actual-version 语义（§11 T02 行；v1.1.3 E3/E4） |
| rt-stack/reference/SPEC.md（T03；task-145 刷新） | 22843 | `f7f1a5337216138b1cffbf10f4a8652bb173f49108256d62790e0050e1b6962f` | block-size/参考语义 |
| rt-stack/evidence/baseline/BASELINE-INVENTORY.md（T01；task-145/task-175 刷新） | 16427 | `e98c0e8fd25570bcb705f5fa60b03242c9969d12331c7814aae95f6b5908cffa` | 基线/回退清单 |
| web/src/routes/auth.rs | 6195 | `689c205c922f152b4057b745dd06088c5781f68690d31f42047023d9210ff8fb` | R02 认证字段 |
| web/src/routes/camilladsp.rs（task-175 刷新） | 6295 | `118efe0efa2e07cfe89e305773ede872cb86efb26e5b41b2bf9f9fec50bcfe5e` | R19 `{db}` 契约 |
| web/src/routes/alsa.rs | 1402 | `c80c04cc4330e0af0946b6c2811e80cd83f3268ab1606adb40cab9cef17352a2` | R28 channel 现状 |
| rt-stack/evidence/baseline/board-evidence/aoip-halfcheck-REPORT-20261116-1949.md | 13646 | `9bdbd14f82470481691393783a45e5cce0c28372a91e64772e7f1d668d181a2c` | AoIP/mode 实测证据（P11/A15/A20；task-145 路径刷新） |
| audio/overlay/etc/systemd/system/tspi-upmix-autodetect.service | — | （路径引用；:4-5 Wants/After、:12 ExecStart） | C07/A11 依赖清理 |

> T02/T04 为 volatile（快照后上游仍在写）；本文件引用只到 §/任务 ID，不依赖其 hash；复审时按 §13.6 复算。本文件自身 hash 不内嵌（自引用）。

---

### 13.2 本任务实际执行的命令与结果

| # | 命令/动作 | 结果 | 备注 |
| --- | --- | --- | --- |
| 1 | `Get-ChildItem rt-stack -Recurse`（写前） | 只有 T03 的 `reference/` 树；`rt-stack/spec` 不存在 | 本文件创建 `rt-stack/spec/`；不写 rt-stack 其它目录 |
| 2 | `Select-String '\.route\(' web/src/routes/mod.rs` | **60** 条 route 条目；方法操作 **67**（含 4 公开） | 用于 §2.3/R 覆盖与 K01 |
| 3 | `Get-FileHash`（§13.1 全部输入，SHA-256） | 全部存在并取得全值 | 输出见 §13.1；无 MISSING |
| 4 | `python calibration-spec/tools/validate_calibration.py examples/…-5.3.2…yml` | exit **0** | 合法 v1 文件通过 |
| 5 | 同上 4.1.2 示例 | exit **0** | 合法 v1 文件通过 |
| 6 | 同上 v11-highshelf 示例 | exit **0** | 合法 v1.1 通过（含 shelf） |
| 7 | `python -c` 内存调用 validator：v1 文件注入 `type: notch` | 返回 error（"type 不认识（当前 'notch'）"） | 证明未知 type 整份拒收策略有实现；未落盘 |
| 8 | 同上：v1.1 注入 `low_shelf` | errors 为空 | 证明 v1.1 shelf 被接受 |
| 9 | 同上：注入未知顶层字段 `unknown_top_key` | warning（未知顶层字段），无 error | 证明未知顶层字段=warning 策略 |
| 10 | `Test-Path` 项目相对路径 | **False**；报告真实路径是**工作区根相对** `C:\WorkSpace\Claw\scratch\lead\aoip-halfcheck\REPORT.md`（真实存在，hash 已记入 §13.1） | 更正：`docs/CURSOR-TASK-AOIP-BLOCKERS.md:15` 为工作区根相对路径；此前“项目相对路径已过期”为误报；本项目根下无该文件 |
| 11 | `git status --porcelain --untracked-files=all -- rt-stack`（只读检查） | rt-stack 整目录未跟踪：含 T01/T02/T03/T04 并行产物与 `spec/core-v1.md`、`spec/fir-toolbox.md`；本任务只新增 `rt-stack/spec/compat-matrix.md`，未改其它文件 | 本任务不 stage/commit；写后核对见 §13.7 自检表 |
| 12 | 未执行：启动 `khaen-audio-web`、curl、WS 握手、cargo test、板端/mode/PTP/AoIP/MOTU 实跑 | — | 见 §13.3 未测项；本文件所有行为结论为静态/他人报告引用 |

### 13.3 未测项（本任务明确未做，禁止当已验证）

1. 未启动 `khaen-audio-web`，未对任何 REST 路径发请求；状态码/字段语义来自源码、API.md 与 `web/tests/api.rs` 静态阅读。
2. 未运行 CamillaDSP、未建立 WS 连接；WS 命令/未知命令/超时行为未实测。
3. 未接板：未执行 mode dante/aes67/off、PTP、AoIP、MOTU/CueMix；AoIP/mode 结论引用他人报告 `aoip REPORT`（2026-11-16 板端实测）。
4. 未运行 `cargo test`（web 测试源存在但未执行；避免写 target 与越界）。
5. calibration 未知 type/未知顶层字段仅通过内存函数调用验证，未用 CLI 文件级跑。
6. 未验证 axum 0.8 对 `/api/*` 未知路径与 serde 提取错误的实际 HTTP 行为（§X01/X03 为静态推断）。
7. 未穷举 `web/assets/app.js` 所有字段依赖（仅抓取主要调用点/字段），UI 字段级兼容以 T70 联调为准。
8. 未核对板端部署文件实际 hash（除旁证：本地 `tspi-mode` SHA-256 前缀 `c4ec1b88` 与 `audio/BOARD-V13-VALIDATION-2026-11-15.md:6` 记录一致）。
9. 未评估性能/RT 行为（本文件不含 RT 数值门；见 WBS §6）。
10. 未在上游 T02/T04 冻结后重算其 hash（§13.1 已标 volatile）；复审/冻结前按 §13.6 复算并以最终版本为准。

### 13.4 环境与限制

- 环境：Windows；PowerShell；无管理员提权；无外网；无板端访问。
- 文件政策：仅写本文件；其它项目文件只读（read/grep/Get-FileHash）。
- 项目 git 状态含其它工作流未提交修改（audio/overlay、calibrate、web 等），与本任务无关；本任务不 stage/commit。
- 时间：2026-11-16；规划文档日期一致（WBS/DEC task-6）。

---

### 13.5 返修记录（task-38，2026-11-17）

| 评审项 | 级别 | 处置 | 修改位置 |
| --- | --- | --- | --- |
| P0-1 DEC 引用失真（54 处/27 首行/11 空行） | P0 | CLOSED：全文 DEC 引用改 §章节锚点（§0-§10），不写死行号 | 全文 + §13.1 |
| P0-2 WBS 引用/hash 过期（WBS T30/106 错任务） | P0 | CLOSED（task-61 二次）：WBS 引用改任务 ID（Txx）/§n；P08 行旧 WBS 行号改 WBS T60；T30/T74/T72 修正；hash 移入 §13.1 快照 | 全文 + §11 + P08 |
| P0-3 R19 字段错（volume_db vs db） | P0 | CLOSED：改为 `{db}` + 响应 volume_db；引 web/src/routes/camilladsp.rs:207-222、web/assets/app.js:801 | R19 |
| P0-4 R16/R47/X15 状态码未冻结 | P0 | CLOSED（task-61 二次）：R16=ADAPT；R47 GET 200 not_supported/PUT 400 not_supported；X15 一般 410 retired + R47 例外；§2.1 唯一处置；**§2.4 已去掉 R50=ADD 回归**并加类别优先级说明 | §2.1、§2.4、R16/R47、X15、K08/K13/K16 |
| P0-5 主决策/T04 冲突 | P0 | CLOSED（task-61 二次）：A01/R10/R11/K05/K39 增 toslink/target；R45/Y07 改 T56 准入；Y17/K34 对齐 T04 §6.9（28 码随版本刷新，不写死 28 码（以 T04 现行 §6.9 为准；T04 v1.2-draft 为 28 码，不写死码数/行号）/行段）；A19 拆 file/真实 AoIP | 对应行 + §13.1 |
| P1-1 X02/Q03 策略未冻结 | P1 | CLOSED（task-61 二次）：§2.4 重写为“策略类别 vs 处置码”解耦表（ADD= R48/R49；ADAPT=R10-R28/R34-R46；KEEP=R01-R09/R29-R33/R50/R51；RETIRE=R47；R19/R28 字段白名单不改变 ADAPT） | §2.4、X02、K57 |
| P1-2 X03/X14 状态码/版本 | P1 | CLOSED：统一 400；API 版本可选请求头缺省 v1 | X03、X14、R50 |
| P1-3 upmix unit Wants 依赖 | P1 | CLOSED：C07/A11/X20 加 service:4-5 依赖清除+禁用 | C07、A11、X20 |
| P1-4 unit 命名 | P1 | CLOSED：A08/A09/A11 以 overlay tspi-* 为准，web/src/config.rs 标 legacy | A08/A09/A11 |
| P1-5 门清单 | P1 | CLOSED：§1.1 改 7 门（G-RT-foundation P1、G-performance 后续） | §1.1 |
| P1-6 hash 不完整/漂移 | P1 | CLOSED（task-61 二次）：§13.1 快照 00:45；T03 hash 补全 64 位、T04/T02 更新并标 volatile、补 aoip REPORT；快照命令见 §13.6 | §13.1 |
| P1-7 R28 channel | P1 | CLOSED：标注文档字段/当前未实现，要求新栈显式决定；列 §2.4 白名单 | R28、§2.4 |
| P1-8 Q 表质量 | P1 | CLOSED：Q02/Q03/Q04/Q05 改已决/执行项；新增 Q13-Q18 | §12 |
| P2-1 C07 行号 | P2 | CLOSED：改 service:12 ExecStart + :4-5 Wants/After | C07 |
| P2-2 A04 引用 | P2 | CLOSED：A04 去掉 :1510-1524 并改属 A05；全路径 | A04 |
| P2-3 §13.2#10 误报 | P2 | CLOSED（task-61 二次）：更正工作区根路径真实存在；aoip hash 补入 §13.1；#11 改为引用 §13.7 自检表（悬空引用消除） | §13.1、§13.2 #10/#11 |
| P2-4 计数/分组 | P2 | CLOSED（task-61 二次）：exit 0..10=11 值 + restore 单列；保护 P01-P14 14 行移回 §2.3 末尾（§2.4 之前） | §2.3 |
| P2-5 API 漂移 | P2 | CLOSED：API 引用改 §章节锚点；§13.1 记当前 API hash | 全文 + §13.1 |
| P2-6 短路径/行偏 | P2 | CLOSED（task-61 二次）：R02 改 web/src/routes/auth.rs:27-35（password_configured:28）；R07 改 web/src/routes/overview.rs；§13.5 路径同步；R 表短名改全路径；§1.3 service 引用改 :12 | 相关行 |
| P2-7 §11 追溯 | P2 | CLOSED（task-61 二次）：§11 T30 行修正为 3 列（P2-performance 后续，1h 0xrun/64 独立窗，不属 P1）；P08 旧 WBS 行号改 WBS T60；T02 行改“只承接 plan/ack/actual-version” | §11 |

**遗留（DECLINED/待复审）**：无。所有 P0/P1/P2 均有修改；复核命令见 §13.6 与 evidence/T05/T05-FIX-LOG.md。

### 13.6 复核命令（只读）

~~~powershell
cd C:\WorkSpace\Claw\projects\tspi-audio-dsp
# 1) 引用策略：不应再出现 DEC:/WBS:/API.md: 数字行号
rg -n "DEC:[0-9]|WBS:[0-9]|API\.md:[0-9]" rt-stack/spec/compat-matrix.md
# 2) 计数基线：R51/W22/Y24/C9/A22/P14/X22/K60/Q18；矩阵行 164（C09 于 2026-11-17 task-200 新增）
#    R 行计数只数 §3 人类可读表（§3.7 机器对照表 51 行是绑定表，另计不入本数）
$l = Get-Content rt-stack/spec/compat-matrix.md
$cut = ($l | Select-String -SimpleMatch '### 3.7').LineNumber[0] - 1
($l[0..($cut-1)] | Select-String '^\| R[0-9]{2} \|').Count   # 期望 51
($l[$cut..($l.Count-1)] | Select-String '^\| R[0-9]{2} \|').Count   # §3.7 期望 51
# 3) P0 关键字段
rg -n "\{db\}|not_supported|retired|toslink|target_multichannel|E_TGT_" rt-stack/spec/compat-matrix.md
# 4) F-21 落地对照：§3.7 必须 51 行；矩阵路由一致性由测试强制
rg -c "^\| R[0-9]{2} \| (KEEP|ADAPT|RETIRE|ADD)(-DEFERRED)? \| (ROUTED|RETIRED_ROUTE|CROSS_CUTTING|DEFERRED) \|" rt-stack/spec/compat-matrix.md
cargo test -p khaen-control rest matrix_routes -- --nocapture   # 需在 rt-stack 下运行
~~~

### 13.7 task-38 修复自检（2026-11-17）

| # | 检查 | 结果 |
| ---: | --- | --- |
| 1 | 引用数字行号残留：`DEC:[0-9]`/`WBS:[0-9]`/`API\.md:[0-9]` | **0 命中**（修复前 DEC 54 处/27 首行、WBS 42 处、API 83 处） |
| 2 | 矩阵行计数 | R51/W22/Y24/C8/A22/P14/X22 = **163**；K01-K60；Q01-Q18 |
| 3 | R19 字段 | `{db}` + 响应 `volume_db`；引 web/src/routes/camilladsp.rs:207-222、web/assets/app.js:801 |
| 4 | 处置/状态码 | R16=ADAPT；R47 GET 200 not_supported / PUT 400 not_supported；X15 一般 410 retired + R47 例外；§2.1 唯一处置 |
| 5 | 主决策/T04 冲突 | A01/R10/R11/K05/K39 有 toslink/target_multichannel；R45/Y07 有 T56 准入/>16384 拒收；Y17 引 T04 §6 与 E_TGT_*；A19 file/AoIP 拆分 |
| 6 | upmix unit 依赖 | C07/A11/X20 含 `service:4-5` Wants/After 与禁用/清除要求 |
| 7 | 输入快照 | §13.1 快照时间 2026-11-17 00:15；WBS/DEC/API/MIMO/T04/T03/T01+auth/camilladsp/alsa hash 记录；复算见 evidence/T05/T05-FIX-LOG.md |
| 8 | 复审命令 | §13.6 可直接复制；自检结果写入 evidence/T05/T05-FIX-LOG.md；本文件自身最终 hash 不内嵌（见 evidence/任务卡） |

> 结果：上表 1-8 全部 PASS（证据与本文件 hash 见 `rt-stack/evidence/T05/T05-FIX-LOG.md`）；未完成项：无。


### 13.8 task-61 二修自检（2026-11-17）

| # | 检查 | 结果 |
| ---: | --- | --- |
| 1 | §2.4 处置码唯一且分类解耦 | ADD=R48/R49；ADAPT=R10-R28/R34-R46；KEEP=R01-R09/R29-R33/R50/R51；RETIRE=R47；R19/R28 字段白名单不改变 ADAPT；R50 不在 ADD |
| 2 | T02 Q-11 对齐 | §2.3/R13/R14/Q09 统一 runtime 仅 64/128，其它值 400/unsupported；§11 T02 行只承接 plan/ack/actual-version |
| 3 | T04 码表/版本对齐 | Y17/K34 引用 T04 §6.9（v1.2-draft 为 28 码，随版本刷新），不再写死旧码数/旧行段；§13.1 T04/T02 标 volatile |
| 4 | 引用修正 | R02=web/src/routes/auth.rs:27-35（password_configured:28）；R07=web/src/routes/overview.rs；R 表/§11 短名已改全路径；§1.3 service 引用改 :12 |
| 5 | 证据/引用闭合 | §13.1 T03 hash 64 位、补 aoip REPORT、T02/T04 快照；§13.2 #10/#11 与 §13.3 第 10 条对齐；§13.7 代码引号字符已恢复 |
| 6 | 计数保持 | R51/W22/Y24/C8/A22/P14/X22=163；K60；Q18（Q12 已移至 Q11 后） |

> 结果：§4 清单（P0 回归 1 + P1 2 + P1/P2 证据 3 + P2 精度 3）全部关闭；未完成项：无。

### 13.9 F-21 矩阵↔路由一致性自检（2026-11-17，task-184）

> 触发：Cursor 审计 F-21（`compat-matrix.md` R07-R46 多为 ADAPT，但 `crates/control/src/rest.rs` 路由表无对应路径）。处置：逐行落地或诚实降级（本文件 §3.7），并新增自动检查；**本文件覆盖 R16=ADAPT 与 §2.4 旧集合口径（§13.7/§13.8 为历史快照，以本文件头与 §3.7 为准）**。

| # | 检查 | 命令/位置 | 结果 |
| ---: | --- | --- | --- |
| 1 | R01-R51 在 §3.7 各出现一次；处置/落地状态取值合法 | `cargo test -p khaen-control rest`（tests/T42/matrix_routes.rs） | PASS（51/51） |
| 2 | §3 行处置 ⇔ §3.7 处置；`*-DEFERRED` ⇔ `DEFERRED`、`RETIRE` ⇔ `RETIRED_ROUTE`、其余 ⇔ ROUTED | 同上 | PASS |
| 3 | ROUTED/RETIRED_ROUTE 行列出的每条 (method,path) 均在 `RestService::routes()` | 同上 | PASS（ROUTED 13 + RETIRED_ROUTE 1） |
| 4 | `RestService::routes()` 每条路由恰被一个 R 行声明（无孤儿/无重复） | 同上 | PASS（36/36） |
| 5 | 负向用例：缺路由/孤儿路由/DEFERRED 却注册/处置-状态不一致/重复 R 号 | 同上（5 个负向 #[test]） | PASS（全部按预期报错） |
| 6 | 证据归档 | `rt-stack/evidence/cursor-fix-matrix/`（README/COVERAGE/日志/SHA256SUMS/repro.ps1） | 已归档 |

> 结果：F-21 的「矩阵声称的兼容面」与「实际控制面」一致——16 行已落地/横切（ROUTED 13 + RETIRED_ROUTE 1 + CROSS_CUTTING 2），35 行 `ADAPT-DEFERRED`/`KEEP-DEFERRED` 不注册路由（`/api/*` 未知路径 X01 JSON 404）；真实 host/板端仍为 NOT_RUN（T26/T71）。

## 14. 变更记录

| 日期 | 版本 | 变更 |
| --- | --- | --- |
| 2026-11-16 | v0.1 | 首版：R01-R51 / W01-W22 / Y01-Y24 / C01-C08 / A01-A22 / P01-P14 / X01-X22 / K01-K60；manifest 与未测项；12 个未决问题 |
| 2026-11-17 | v0.2 | task-38：按 T05 独立评审（NEEDS-FIX 20 条）修 P0 5/P1 8/P2 7；DEC/WBS/API 引用改路径+§/任务 ID；R19 `{db}`；R16=ADAPT、R47/X15 状态码；A01 增 toslink/target；R45/Y07 T56 准入；Y17 对齐 T04；新增 §2.4、§13.1 快照、§13.5/§13.6；详见 evidence/T05 |
| 2026-11-17 | v0.3 | task-61 二修：§2.4 策略/处置码解耦（去掉 R50=ADD 回归、R19/R28 字段白名单不降级）；R02/R07 路径与行修正、短名全路径；T02 Q-11 统一 runtime chunksize=64/128（其它 400/unsupported）；T04 28 码/§6.9 同步（不写死码数/行号）、T03 hash 补全、T02/aoip 快照；§13.1/13.2/13.3/13.7/13.8 更新 |
| 2026-11-17 | v0.5 | task-200：新增 C09「缺中置内容适配（P1 ADD，不是上混/通用下混）」——输出布局无 C 且有 C 内容时在 plan 前把 C −3 dB 预混进 FL/FR 并消费 C 源；有中置布局 passthrough；D7 headroom 不足等量 trim；矩阵行 163→164（详见 rt-stack/evidence/center-downmix/） |
| 2026-11-17 | v0.4 | task-184（Cursor F-21）：新增 §3.7 R01-R51↔`rest.rs` 路由落地对照（机器可读，51 行）；R07-R09/R15-R46 共 35 行改 `ADAPT-DEFERRED`/`KEEP-DEFERRED`（P1 不注册路由，逐行理由/owner）；§1.3/§2.1/§2.3/§2.4/§11/§12(Q01/Q13) 同步；新增 §13.9 与 `matrix_routes` 自动检查（含 5 条负向用例）；详见 evidence/cursor-fix-matrix |
| 2026-11-17 | v0.6 | task-206（Cursor CLI v1.0 N-02 文档口径）：§0 边界 1 改为「P1 不实现**通用**上混/下混」+ 唯一例外 C09（用户要求项、已实现、缺音箱保护，不是通用/节目 downmix）；§6 结论补例外指引；C09 行标签由「P1 ADD」改为「P1 必须/用户要求项/已实现」并补 host 接线 task-203 状态。纯文档澄清，不改实现/判据；证据见 evidence/cursor-fix-v1-evidence/ |
| 2026-11-17 | v0.7 | task-210（Cursor v1.1 N-10 分层硬约束）：中置下混内容适配迁入 `rt-host` 自持（`crates/rt-host/src/content_map.rs`，含 T57 overlay 最小严格读取器；行为与 task-200/203 一致），rt-host 移除 `khaen-compat` 依赖与调用；compat::channel_map 保留为旧 S1/兼容层参考；C09 注记改为「host 自持 + 已接线」（删除 task-203 的临时进行中戳）；证据 `rt-stack/evidence/cursor-fix-v1-hostmap/` |


