# tests/T70 —— 真实 REST/WS Web 入口与页面私有测试

- 任务：WBS T70（deps T42/T24/T60/T66，均已完成）；写域 "crates/control/src/web.rs" + "tests/T70/" + "evidence/T70/"。
- 权威输入：WBS T70 行与 §1/§6/§7；DSP-HOST-DECISION §6；"rt-stack/spec/compat-matrix.md"
  R01-R51/§4（W01-W22）/K01/K02/K05/K14/K15/K53/K57/X01/X14/X15/X21/X22；"rt-stack/README.md" §3/§5；
  T42 "crates/control/src/rest.rs"、T24 "crates/compat/src/ws.rs"、T60 "crates/calibration/src/job.rs"、
  T66 "crates/calibration/src/compile_apply.rs"。
- 测试文件："web_contract.rs"（40 tests；含真实 TCP loopback HTTP+WS fake web 端到端交互）。
- 运行："cargo test -p khaen-control web"（或加 "--offline"；全部离线，不打开设备、不启外部服务、不跑板端）。
- 证据："rt-stack/evidence/T70/"（真实执行日志、SAMPLE 响应、页面快照、manifest）。

## 注册方式（不要改 public_registration.rs）

cargo 不自动发现 "tests/<Txx>/*.rs"，公共注册属 integrator 独占。因此本文件由
"crates/control/src/web.rs" 文件末的 "#[cfg(test)]" 子模块通过 include! 编译执行：

    #[cfg(test)]
    mod web_tests {
        use super::*;
        include!("../tests/T70/web_contract.rs");
    }

本任务未改 Cargo.toml / Cargo.lock / lib.rs / public_registration.rs / CLI / README / spec；
未新增第三方依赖；未碰板端/服务/模式；未执行任何 git 命令；只 rustfmt 自己的两个文件。

## 被测目标（web.rs）

- HTTP/1.1 传输（纯 std）：请求行/头/Content-Length/keep-alive/限量；静态资源 + REST 反向代理；
  "/ws" 为 RFC 6455 升级入口；连接上限/IO 超时/有界请求数。
- 静态页面："/"、"/index.html"、"/style.css"、"/app.js"、"/favicon.*" 全部内嵌；页面按钮全部经
  真实 REST/WS 调用完成操作（状态/模式/音量/路由/格式/校准 start·cancel·apply·retest·rollback）。
- REST 接线："/api/*" 与 "/healthz" 全量交给 T42 "RestService"（统一 envelope、鉴权、API 版本、
  prepare->commit actual ack 全部复用，Web 层不另起语义）。
- WS 接线：T24 "WsSession" + 本层 "WebWsContext"/"RestWsPlane"：
  - 标准命令（GetVersion/GetState/GetVolume/GetMute/SetVolume/SetMute/GetCaptureRate/
    GetClippedSamples/GetConfigVersion/GetConfig/SetConfigJson/PrepareConfigJson/CommitConfig/
    AbortConfig 等）经 T42 REST actual 状态/两阶段 commit 生效；
  - T70 扩展命令 "GetCalibrationJobs"/"GetCalibrationJob"（T24 冻结集之外，保持 W02/W03 同键回复），
    值为 T42 R48 校准 job 投影（状态/阶段机器名与 T60 JobState/JobStage 一致）。
- 鉴权：WS 升级与 REST 同一套 token/header 或 Cookie 会话；浏览器 Cookie 会话的写命令由 Web 层
  服务端补 CSRF（浏览器 WebSocket API 不能设自定义头）；未认证升级 401。

## fake web 端到端交互用例（"真实页面"验收口径）

- 后端替身：T42 "MemoryEngine" + 测试内 "SimCalibration"（"CalibrationPort" 实现，模拟
  T60 状态机 queued→preflight→…→prepared 与 T66 apply/retest/rollback 回执语义：
  apply 只在 prepared 后返回 applied_version；retest 只在 applied 后创建新 job；
  rollback 只在有 last-good 时返回 last_good_version；cancelled/rolled_back 终态与 T42 REST 一致）。
- 传输：真实 "TcpListener"（127.0.0.1:0）+ 手写 HTTP/1.1 客户端 + 手写 RFC 6455 帧客户端，
  覆盖 REST 401/404/405/413/409/stale、prepare→commit actual version、volume/mute、mode 202、
  audio format 64/128、routing role->DAC + prepare/commit、校准全生命周期、WS 握手 101
  （RFC 6455 测试向量）、GetState/Volume/Mute/CaptureRate/Clipped/ConfigVersion、
  SetVolume/SetMute、SetConfigJson（prepare+commit）、Prepare/Abort/Commit、未知命令、
  Binary 帧显式错误、Ping/Pong、Close、校准进度扩展、Cookie 会话+服务端 CSRF。
- 页面契约：断言页面含全部交互按钮（data-action）、JS 实际驱动全部 REST 端点与 WS 命令、
  无外部资源/无 inline script/style/on*/eval/innerHTML（CSP 同源纪律）。
- 纯函数用例：WS 帧分片/掩码/协议违规（1002/1009）、SHA-1/base64 向量、
  in-memory "run_ws_connection" 直驱同一 T42 REST 核心。

## 覆盖分组（40 tests）

| 组 | 覆盖 |
|---|---|
| A 页面/资源 6 | 交互按钮/状态挂点、无外部资源/无 inline、JS 驱动端点与 WS 命令、内嵌资源哈希、静态路由 UI 安全头、非 GET 静态 405 |
| B HTTP 传输 7 | 请求行/头/体解析、absolute-form/Connection、畸形请求 400/405/413/417/431、EOF/keep-alive、传输错误 envelope、连接上限 503 |
| C REST 接线 9 | 匿名 healthz + 401 鉴权、状态 actual 字段、JSON 404/405/统一版本/envelope、volume/mute actual ack、mode 202、audio format prepare/commit、routing prepare/commit actual ack + replay、commit 无 prepare 409/stale |
| D 校准交互 6 | 无 port 503 不伪造、full 生命周期（start→poll prepared→apply→retest→rollback→list）、cancel、apply/retest 未 prepared 409、未知 job/bad kind、T60 状态词汇一致 |
| E WebSocket 10 | 握手 SHA-1/base64 RFC 向量、帧编解码/分片/Ping 交织、协议违规 1002/1009、升级鉴权 401、遥测+音量 actual ack、SetConfigJson/Prepare/Abort/Commit actual、未知命令+Ping/Binary/Close、校准进度扩展、Cookie 会话+服务端 CSRF 写、in-memory 传输 |
| F 横切 2 | UI/API 双 CSP 口径、WS 扩展命令契约与 T24 冻结集边界、status schema/limits/input-mode 冻结码 |

## 明确 NOT_RUN（禁止当已验证）

- 未上板、未打开 ALSA/MOTU/UMIK、未启动真实 T26 host/engine 与 T60 校准 daemon；
  WS 的 RT 测量量（processing_load/buffer_level/rate_adjust/RMS/fader）在 REST 桥中按 E-3
  保守返回不可测值，真实值由 T26 注入 "with_plane_factory" 的 WsControlPlane 提供。
- 未做真实浏览器视觉/交互回归（无 headless browser 执行）；页面正确性以 HTTP/WS 真实传输测试
  + 页面契约断言 + "node --check" 语法检查（evidence/T70/node-syntax.log）为准。
- 未验证 TLS/HTTP2/反代、未做 80 端口部署（T26/T74）。
