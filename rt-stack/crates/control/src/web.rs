//! [NONRT] 真实 REST/WS Web 入口与页面（WBS T70；compat K01/K02/K05/K14/K15/K53/K57 + R51 + DEC §6） ——WBS T70 写域。
//!
//! 本模块是新栈**唯一 Web 入口**：把 T42 传输无关的 REST 控制核心（crate::rest::RestService）
//! 与 T24 WS 协议引擎（khaen_compat::ws::WsSession）接到真实 HTTP/1.1 与 RFC 6455 WebSocket
//! 传输上，并提供可直接日常操作的页面（输入/路由/volume/mute/状态 + 校准
//! start/cancel/apply/retest/rollback 全交互）；不是占位按钮，也不是只给 curl 的 API。
//!
//! ## 权威输入（只读）
//! - WBS T70 行与 §1/§6/§7（scratch/lead/planner/KHAEN-RT-STACK-WBS.md）。
//! - DSP-HOST-DECISION §6（P1 I/O/control/Web 入口完整性：自有 REST/WS + 真实 Web 页面）。
//! - rt-stack/spec/compat-matrix.md R01-R51、§4（W01-W22）、K01/K02/K05/K14/K15/K53/K57、
//!   X01/X14/X15/X21/X22、§6（S1 REST/WS/页面兼容面）。
//! - rt-stack/README.md §3（写域纪律）与 tests/T42/README.md、tests/T24/README.md（include! 自注册）。
//! - T42 crates/control/src/rest.rs（RestService/RestRequest/RestResponse/JsonValue/CAL_JOB_STATES）、
//!   T24 crates/compat/src/ws.rs（WsSession/WsControlPlane/WsReply/PreparedConfig/CommitAck）、
//!   T60 crates/calibration/src/job.rs（JobState/JobStage 机器名）、
//!   T66 crates/calibration/src/compile_apply.rs（apply/retest/rollback 回执语义）。
//!
//! ## 逐条落点
//! - R51/K01：GET /、/index.html、/favicon.*、/theme-init.js、/assets/* 公开静态（Vue 控制台
//!   构建产物，见 E-7）；未命中静态表且不是 /api 的路径回退 `index.html`（SPA history 路由）；
//!   /api/* 全量转 RestService（含统一 envelope / 404 / 405 / API 版本头）；/ws 为
//!   WS 升级入口（不是 REST 路由）。旧内嵌页的 /style.css、/app.js 与 /calibration*
//!   （手写校准向导）仍在，只是不再是首页。
//! - K02/K05/K14/K15/K53/K57 + DEC §6：页面实际驱动 GET/PUT /api/runtime/{status,config,volume,
//!   mute,target,coeff,mode}、POST /api/runtime/{prepare,commit}、GET/PUT /api/mode、
//!   GET/PUT /api/audio/format、GET /api/auth/*、GET/POST /api/calibration/jobs 与
//!   /api/calibration/jobs/{id}/{cancel,apply,retest,rollback}；所有写操作走 T42 的 actual ack，
//!   页面显示 actual_version/graph_id/epoch，不把 202/204 当已生效；响应体 synthetic=true
//!   （内存 engine 未接真实 host）时页面必须显示 synthetic 而不是 actual（F-13/F-28）。
//! - T24 W01-W22：/ws 是 RFC 6455 文本 JSON 帧传输，帧内容交给 WsSession（W02/W03/W04/W20/W21）；
//!   Binary 帧由 WsSession 显式报错；Ping 回 Pong；Close 正常关闭；GetVersion/GetState/GetVolume/
//!   GetMute/GetCaptureRate/GetClippedSamples/GetConfigVersion/GetConfig 等读命令直接反映
//!   T42 REST actual 状态；SetVolume/SetMute 写命令经 T42 REST actual ack；SetConfigJson/
//!   PrepareConfigJson/CommitConfig/AbortConfig 走 T42 prepare->commit 两阶段（复用
//!   ws::validate_commit_ack，不假 ack）。
//! - DEC §6 校准进度：T24 冻结命令集无校准命令，故本层追加 **T70 WS 扩展命令**
//!   GetCalibrationJobs / GetCalibrationJob（同 W02/W03 同键回复），值为 T42 R48 校准 job 投影；
//!   job 状态/阶段机器名与 T60 JobState/JobStage 一致（REST 层已按 CAL_JOB_STATES 校验）。
//! - 页面：HTML/CSS/JS 全部内嵌（无外部资源、无 CDN、无 inline script/style/on* 属性），
//!   CSP 仅允许 same-origin script/style/img 与 same-origin fetch/WS；按钮全部通过真实
//!   REST/WS 调用完成操作（start/cancel/apply/retest/rollback、路由编辑+prepare+commit、
//!   volume/mute、模式切换、格式切换），错误按 R50 envelope 的 code/message 显示。
//!
//! ## 契约偏差/边界（证据见 evidence/T70/MANIFEST.md）
//! - E-1：本 crate 无第三方依赖（README §2 只允许 rt-host Linux target 的 alsa），HTTP/1.1
//!   解析、SHA-1/base64（WS 握手）、RFC 6455 帧编解码全部用纯 std 自带实现；不新增依赖。
//! - E-2（task-226 更新）：daemon 已把真实 T60 JobManager 适配（calibration_job_port.rs，
//!   离线合成 backend）注入 T42 CalibrationPort；本模块只消费 REST 投影与页面交互（不直接
//!   持有 JobManager），REST 响应带 engine/synthetic，页面显式显示 synthetic 而非 actual，
//!   未接线时 T42 显式 503（本模块不伪造 job）。
//! - E-3：WS 兼容面（T24 trait）在本模块用 **REST 桥**（RestWsPlane）实现：宿主真实 RT 状态
//!   （load/buffer/rate_adjust/RMS/fader）不能从 REST 快照得到，桥按保守口径返回不可测值
//!   （processing_load/buffer_level 为 0、rate_adjust 显式不可用 `rate_adjust_available=false`、
//!   RMS 为 T24 约定的静音 floor、fader 未分配）；T26 用真实 host/engine 实现 WsControlPlane
//!   后由 with_plane_factory 注入替换，页面/传输不变。
//! - E-4：Web 层不实现 TLS/HTTP2；P1 板端由内网反代或直接 HTTP 提供（DEC §6 未要求 TLS）。
//!   WS 鉴权 = 与 REST 同一套 token/Cookie 会话（浏览器用 Cookie+服务端补 CSRF；非浏览器可用
//!   X-KHAEN-Token 头或 ?token= query 便于脚本/测试），未认证升级直接 401。
//! - E-5：静态资源服务 HEAD 未实现（405）；页面不使用 HEAD。Content-Length/chunked：仅支持
//!   Content-Length 请求体，Transfer-Encoding 显式 400（浏览器 fetch 默认 Content-Length）。
//! - E-6（task-245）：accept 循环是**阻塞/事件驱动**（空闲时线程睡在 accept(2)，0 轮询、
//!   0 空闲唤醒、CPU≈0；有连接立即被唤醒）。停止契约 = 置位 stop 后调用 `WebServer::wake()`
//!   自连接打断阻塞 accept（main 的 SIGTERM/--exit-after-ms 与 T70 TestServer 均按此接线）。
//!   修复前监听 socket 非阻塞 + 紧循环轮询，空闲时烧掉约 1 个核（板端实测 93% 单核）。
//! - E-7（控制台换 Vue）：daemon 直接托管仓库 `webui/` 里的 Vue 3 + Element Plus 控制台。
//!   构建产物 `webui/dist`（index.html、favicon.svg、theme-init.js、assets/*.js|*.css）提交在
//!   仓库里，由 `crates/control/build.rs` 在编译期扫描并生成 `crate::webui_assets` 静态表
//!   （`include_bytes!` 编进二进制，运行期不读磁盘、不引入 axum/tokio）。路由：静态表命中即直出，
//!   未命中且非 `/api` 回退 index.html。`/assets/*`（文件名带内容 hash）长缓存
//!   `public, max-age=31536000, immutable`，index.html/favicon/theme-init 一律 `no-store`。
//!   静态资源仍**不需要**登录（与 K01 一致），`/api/*` 仍需会话。CSP 因 Element Plus 需要内联
//!   `style` 属性而单独用 `SPA_CONTENT_SECURITY_POLICY`（`style-src 'self' 'unsafe-inline'`；
//!   脚本仍只允许同源外置文件，无内联 script / 无 eval）；旧内嵌页与校准向导仍用严格版
//!   `UI_CONTENT_SECURITY_POLICY`。
//!
//! ## 测试（include! 自注册，integrator 不要再写 public_registration.rs）
//! 私有测试在 crates/control/tests/T70/web_contract.rs，经文件末
//! #[cfg(test)] mod web_tests { include!("../tests/T70/web_contract.rs"); } 进入 lib 单测构建；
//! 运行 cargo test -p khaen-control web。全部离线：真实 TCP loopback + MemoryEngine + 测试内
//! SimCalibration 假后端（模拟 T60 状态机/T66 apply/retest/rollback 回执），不碰板端/设备。

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use khaen_compat::ws;

use crate::rest::{JsonValue, Method, RestRequest, RestResponse, RestService, MAX_BODY_BYTES};
use crate::webui_assets;

// ---------------------------------------------------------------------------
// 常量（R51/K01/K02 + DEC §6 + T24 W01-W22）
// ---------------------------------------------------------------------------

/// 页面 schema（页面自身版本身份；R51 静态资源）。
pub const WEB_PAGE_SCHEMA: &str = "khaen-web/v1";
/// Web 层版本（显示用；不是 REST API 版本）。
pub const WEB_VERSION: &str = env!("CARGO_PKG_VERSION");
/// WebSocket 入口路径（T24 传输；浏览器 new WebSocket(location.origin.replace('http','ws') + /ws)）。
pub const WS_PATH: &str = "/ws";
/// 首页路径（R51；Vue 控制台 index.html，见 E-7）。
pub const INDEX_PATH: &str = "/";
/// 首页别名（R51；Vue 控制台 index.html）。
pub const INDEX_HTML_PATH: &str = "/index.html";
/// 样式表路径（R51）。
pub const STYLE_CSS_PATH: &str = "/style.css";
/// 页面脚本路径（R51）。
pub const APP_JS_PATH: &str = "/app.js";
/// favicon（R51 /favicon.*；Vue 控制台的 favicon.svg，见 E-7）。
pub const FAVICON_SVG_PATH: &str = "/favicon.svg";
/// favicon 兼容别名（R51 /favicon.*；浏览器默认请求 /favicon.ico 时给内嵌 SVG）。
pub const FAVICON_ICO_PATH: &str = "/favicon.ico";
/// 校准向导页面路径（正式入口；源文件 crates/control/ui-src/calibration，构建到 control/assets）。
pub const CALIBRATION_PAGE_PATH: &str = "/calibration";
/// 校准向导样式路径。
pub const CALIBRATION_CSS_PATH: &str = "/calibration/style.css";
/// 校准向导脚本路径。
pub const CALIBRATION_JS_PATH: &str = "/calibration/app.js";
/// 校准向导 HTML（构建产物：node ui-src/calibration/src/build.js）。
pub const CALIBRATION_HTML: &str = include_str!("../assets/calibration/index.html");
/// 校准向导样式表（构建产物）。
pub const CALIBRATION_CSS: &str = include_str!("../assets/calibration/style.css");
/// 校准向导脚本（构建产物；含 pilot 数据与 level/delay/bass 步骤）。
pub const CALIBRATION_JS: &str = include_str!("../assets/calibration/app.js");
/// 页面 CSP（同源脚本/样式/图片 + same-origin connect；无 inline/无外部资源）。
///
/// 用于手写内嵌页与 /calibration 校准向导（两者都没有内联 style/script）。
pub const UI_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// Vue 控制台 CSP（E-7）：Element Plus 用 `style` 属性驱动进度条/抽屉/表格，故 `style-src`
/// 必须允许内联样式；脚本仍**只**允许同源外置文件（无 'unsafe-inline'、无 'unsafe-eval'，
/// 本包唯一脚本是外置的 /theme-init.js 与带 hash 的 /assets/*.js）。
pub const SPA_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// 默认监听地址（S1 Web 默认 0.0.0.0:80，task-18；板端由 T26/T74 实际启动）。
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:80";
/// 请求头上限（含请求行）。
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// 请求头条数上限。
pub const MAX_HEADER_COUNT: usize = 100;
/// 单连接最大请求数（keep-alive 有界）。
pub const MAX_REQUESTS_PER_CONNECTION: u64 = 1000;
/// 单连接 accept/handler 上限（超出返回 503；process/thread 有界）。
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;
/// HTTP/WS 连接 I/O 超时（秒）。
pub const DEFAULT_IO_TIMEOUT_SECS: u64 = 30;
/// WS 单帧上限（与 T24 DEFAULT_MAX_FRAME_BYTES 一致）。
pub const WS_MAX_FRAME_BYTES: usize = ws::DEFAULT_MAX_FRAME_BYTES;
/// WS 关闭原因文本上限（RFC 6455 控制帧 125 字节内）。
pub const WS_CLOSE_REASON_MAX: usize = 120;

/// T70 WS 扩展命令：校准进度（T24 冻结命令集之外，见模块文档）。
pub const WS_CAL_COMMANDS: [&str; 2] = ["GetCalibrationJobs", "GetCalibrationJob"];
/// T70 WS 扩展命令两个命令的线上 schema（值对象）。
pub const WS_CAL_SCHEMA: &str = "khaen-ws-cal/v1";

/// 传输层错误码（R50 envelope 复用；非 /api 路径也返回 JSON envelope）。
pub const CODE_INVALID_REQUEST: &str = "invalid_request";
/// 未知/不支持的方法（405）。
pub const CODE_METHOD_NOT_ALLOWED: &str = "method_not_allowed";
/// 请求体/帧超上限（R50/X22）。
pub const CODE_PAYLOAD_TOO_LARGE: &str = "payload_too_large";
/// 显式不支持（Transfer-Encoding 等）。
pub const CODE_UNSUPPORTED_VALUE: &str = "unsupported_value";
/// 服务不可用（连接数上限）。
pub const CODE_SERVICE_UNAVAILABLE: &str = "service_unavailable";
/// 内部错误。
pub const CODE_INTERNAL_ERROR: &str = "internal_error";

/// 静态资源响应头（UI 版：CSP 允许同源脚本/样式；其余与 T42 SECURITY_HEADERS 同口径）。
pub const UI_SECURITY_HEADERS: [(&str, &str); 4] = [
    ("Content-Security-Policy", UI_CONTENT_SECURITY_POLICY),
    ("X-Frame-Options", "DENY"),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
];

/// Vue 控制台静态资源响应头（E-7；与 `UI_SECURITY_HEADERS` 同口径，CSP 换 SPA 版）。
pub const SPA_SECURITY_HEADERS: [(&str, &str); 4] = [
    ("Content-Security-Policy", SPA_CONTENT_SECURITY_POLICY),
    ("X-Frame-Options", "DENY"),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
];

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// WS 升级鉴权模式（E-4；默认与 REST 同一套 token/Cookie 会话）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsAuthMode {
    /// 必须通过 REST 同一套鉴权（token 头 / Cookie 会话）；未认证升级 401。
    RequireAuth,
    /// 不做传输层鉴权（仅限本机回环/受信反代部署；必须显式设置，不静默放宽）。
    AllowAnonymous,
}

impl Default for WsAuthMode {
    fn default() -> Self {
        WsAuthMode::RequireAuth
    }
}

/// Web 服务配置（每 server 一份；T26/T74 可注入）。
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// 监听地址（默认 0.0.0.0:80；测试用 127.0.0.1:0）。
    pub bind_addr: String,
    /// 并发连接上限（超出 503；有界，不无界起线程）。
    pub max_connections: usize,
    /// 读/写 I/O 超时（秒）。
    pub io_timeout_secs: u64,
    /// WS 升级鉴权模式。
    pub ws_auth: WsAuthMode,
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig {
            bind_addr: DEFAULT_BIND_ADDR.to_string(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            io_timeout_secs: DEFAULT_IO_TIMEOUT_SECS,
            ws_auth: WsAuthMode::RequireAuth,
        }
    }
}

impl WebConfig {
    /// 回环随机端口（测试/本地开发；端口 0 由内核分配）。
    pub fn loopback_ephemeral() -> Self {
        WebConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            ..WebConfig::default()
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP 请求/响应类型（R51；纯 std HTTP/1.1 子集）
// ---------------------------------------------------------------------------

/// 已解析的 HTTP 请求（body 已按 Content-Length 读全；path 已百分号解码、query 已分项）。
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// 方法（GET/POST/PUT/DELETE；HEAD/OPTIONS/TRACE 在解析时按 405 拒绝）。
    pub method: Method,
    /// 已解码路径（以 / 开头；不含 query/fragment）。
    pub path: String,
    /// query 键值（百分号解码；重复键保留顺序）。
    pub query: Vec<(String, String)>,
    /// 请求头（名称原样；查找大小写不敏感）。
    pub headers: Vec<(String, String)>,
    /// 请求体原始字节（上限 MAX_BODY_BYTES）。
    pub body: Vec<u8>,
    /// 本连接是否保持（HTTP/1.1 默认 true；Connection: close 为 false）。
    pub keep_alive: bool,
    /// HTTP 版本（"HTTP/1.1" / "HTTP/1.0"）。
    pub version: String,
}

impl HttpRequest {
    /// 请求头查找（大小写不敏感；重复头取第一个）。
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 是否 WS 升级请求（Upgrade: websocket + Connection 含 upgrade；大小写不敏感）。
    pub fn is_websocket_upgrade(&self) -> bool {
        let upgrade = self
            .header_value("Upgrade")
            .map(|value| value.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
        let connection = self
            .header_value("Connection")
            .map(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
            .unwrap_or(false);
        upgrade && connection
    }

    /// 转成 T42 REST 请求（路径/query/头/体原样；鉴权语义完全复用 RestService）。
    pub fn to_rest_request(&self) -> RestRequest {
        RestRequest {
            method: self.method,
            path: self.path.clone(),
            query: self.query.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
        }
    }
}

/// HTTP 响应（body 为字节；JSON/文本/静态资源）。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头。
    pub headers: Vec<(String, String)>,
    /// 响应体。
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// 空响应（无头无体）。
    pub fn new(status: u16) -> Self {
        HttpResponse {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// JSON 响应（application/json; charset=utf-8）。
    pub fn json(status: u16, body: &str) -> Self {
        Self::bytes(
            status,
            "application/json; charset=utf-8",
            body.as_bytes().to_vec(),
        )
    }

    /// 纯文本响应（text/plain; charset=utf-8）。
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self::bytes(
            status,
            "text/plain; charset=utf-8",
            body.into().into_bytes(),
        )
    }

    /// 指定 Content-Type 的字节响应。
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        let mut response = HttpResponse::new(status);
        response
            .headers
            .push(("Content-Type".to_string(), content_type.to_string()));
        response.body = body;
        response
    }

    /// 追加响应头（builder）。
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        set_header(&mut self.headers, name, value);
        self
    }

    /// 响应头查找（大小写不敏感）。
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// R50 错误 envelope（transport 层；code 使用稳定字符串）。
    pub fn error(status: u16, code: &str, message: impl Into<String>) -> Self {
        let value = JsonValue::obj(vec![(
            "error",
            JsonValue::obj(vec![
                ("code", JsonValue::str(code)),
                ("message", JsonValue::str(message.into())),
            ]),
        )]);
        HttpResponse::json(status, &value.to_json())
    }

    /// 由 T42 REST 响应转换（状态码/头/体原样；不重写 envelope）。
    pub fn from_rest(response: RestResponse) -> Self {
        HttpResponse {
            status: response.status,
            headers: response.headers,
            body: response.body.into_bytes(),
        }
    }
}

/// 设置/覆盖响应头（大小写不敏感替换）。
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(entry) = headers
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        entry.1 = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

/// HTTP 层错误（解析/限制错误；转成 R50 envelope 响应）。
#[derive(Debug, Clone)]
pub struct HttpError {
    /// HTTP 状态码。
    pub status: u16,
    /// 稳定错误码。
    pub code: &'static str,
    /// 可读消息（已脱敏）。
    pub message: String,
}

impl HttpError {
    /// 构造。
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        HttpError {
            status,
            code,
            message: message.into(),
        }
    }

    /// 转成响应（405 自动带 Allow）。
    pub fn into_response(self) -> HttpResponse {
        let mut response = HttpResponse::error(self.status, self.code, self.message);
        if self.status == 405 {
            response = response.with_header("Allow", "GET, POST, PUT, DELETE");
        }
        response
    }
}

/// 路由结果：普通响应或 WS 升级。
#[derive(Debug, Clone)]
pub enum WebDispatch {
    /// 立即写回该响应。
    Response(HttpResponse),
    /// 101 升级：accept key 与桥接 REST 所需鉴权头（E-4）。
    WebSocket {
        /// Sec-WebSocket-Accept 值。
        accept_key: String,
        /// 桥接 REST 用的鉴权头（Cookie/token/CSRF）。
        auth_headers: Vec<(String, String)>,
    },
}

// ---------------------------------------------------------------------------
// URL/百分号解码（R51 路径；X01 拒绝畸形）
// ---------------------------------------------------------------------------

/// 百分号解码（plus_is_space=true 时 '+' -> ' '）；非法编码/非 UTF-8 返回 None。
pub fn percent_decode(text: &str, plus_is_space: bool) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                out.push((high << 4) | low);
                index += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// 十六进制字符值。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 拆分 query 为键值对（键/值均百分号解码；空键保留）。
pub fn parse_query(raw: &str) -> Result<Vec<(String, String)>, HttpError> {
    let mut out = Vec::new();
    if raw.is_empty() {
        return Ok(out);
    }
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        let key = percent_decode(key, true).ok_or_else(|| {
            HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                "invalid percent-encoding in query key",
            )
        })?;
        let value = percent_decode(value, true).ok_or_else(|| {
            HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                "invalid percent-encoding in query value",
            )
        })?;
        out.push((key, value));
    }
    Ok(out)
}

/// 规范化 request-target：接受 origin-form（/path?query）与 absolute-form（http://host/path）。
/// 返回 (path, query)；path 已百分号解码（fragment 不发送，出现即截断）。
pub fn parse_target(target: &str) -> Result<(String, Vec<(String, String)>), HttpError> {
    let raw = if let Some(rest) = target.strip_prefix("http://") {
        match rest.find('/') {
            Some(index) => &rest[index..],
            None => "/",
        }
    } else if let Some(rest) = target.strip_prefix("https://") {
        match rest.find('/') {
            Some(index) => &rest[index..],
            None => "/",
        }
    } else {
        target
    };
    let raw = match raw.find('#') {
        Some(index) => &raw[..index],
        None => raw,
    };
    if !raw.starts_with('/') {
        return Err(HttpError::new(
            400,
            CODE_INVALID_REQUEST,
            "request target must be an absolute path",
        ));
    }
    let (path_raw, query_raw) = match raw.split_once('?') {
        Some((path, query)) => (path, query),
        None => (raw, ""),
    };
    let path = percent_decode(path_raw, false).ok_or_else(|| {
        HttpError::new(
            400,
            CODE_INVALID_REQUEST,
            "invalid percent-encoding in request path",
        )
    })?;
    if path.is_empty() || !path.starts_with('/') {
        return Err(HttpError::new(
            400,
            CODE_INVALID_REQUEST,
            "request path must start with /",
        ));
    }
    let query = parse_query(query_raw)?;
    Ok((path, query))
}

// ---------------------------------------------------------------------------
// HTTP/1.1 请求解析（纯 std；Content-Length 子集，E-5）
// ---------------------------------------------------------------------------

/// 读取一个 HTTP 请求；连接正常关闭返回 Ok(None)。
pub fn read_http_request<R: BufRead>(reader: &mut R) -> Result<Option<HttpRequest>, HttpError> {
    let mut head_bytes = 0usize;
    let mut line = Vec::new();
    let request_line = loop {
        line.clear();
        let read = read_line(reader, &mut line)?;
        if read == 0 {
            return Ok(None);
        }
        head_bytes += read;
        if head_bytes > MAX_HEAD_BYTES {
            return Err(HttpError::new(
                431,
                CODE_PAYLOAD_TOO_LARGE,
                "request head exceeds limit",
            ));
        }
        let text = std::str::from_utf8(&line).map_err(|_| {
            HttpError::new(400, CODE_INVALID_REQUEST, "request line is not valid UTF-8")
        })?;
        let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
        if !trimmed.is_empty() {
            break trimmed.to_string();
        }
        // 跳过请求行前空行（RFC 7230 允许接收端忽略）。
    };
    let mut parts = request_line.split(' ').filter(|part| !part.is_empty());
    let method_text = parts
        .next()
        .ok_or_else(|| HttpError::new(400, CODE_INVALID_REQUEST, "missing request method"))?;
    let target = parts
        .next()
        .ok_or_else(|| HttpError::new(400, CODE_INVALID_REQUEST, "missing request target"))?;
    let version = parts
        .next()
        .ok_or_else(|| HttpError::new(400, CODE_INVALID_REQUEST, "missing HTTP version"))?;
    if parts.next().is_some() {
        return Err(HttpError::new(
            400,
            CODE_INVALID_REQUEST,
            "request line has too many fields",
        ));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpError::new(
            505,
            CODE_INVALID_REQUEST,
            "unsupported HTTP version",
        ));
    }
    if method_text == "HEAD" || method_text == "OPTIONS" || method_text == "TRACE" {
        return Err(HttpError::new(
            405,
            CODE_METHOD_NOT_ALLOWED,
            "method not supported by the KHAEN web entry",
        ));
    }
    let method = Method::parse(method_text).ok_or_else(|| {
        HttpError::new(
            405,
            CODE_METHOD_NOT_ALLOWED,
            "method not supported by the KHAEN web entry",
        )
    })?;
    let (path, query) = parse_target(target)?;

    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        line.clear();
        let read = read_line(reader, &mut line)?;
        if read == 0 {
            return Err(HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                "connection closed inside request head",
            ));
        }
        head_bytes += read;
        if head_bytes > MAX_HEAD_BYTES {
            return Err(HttpError::new(
                431,
                CODE_PAYLOAD_TOO_LARGE,
                "request head exceeds limit",
            ));
        }
        let text = std::str::from_utf8(&line).map_err(|_| {
            HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                "request header is not valid ASCII",
            )
        })?;
        let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with(' ') || trimmed.starts_with('\t') {
            return Err(HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                "obsolete header line folding is not supported",
            ));
        }
        let (name, value) = trimmed.split_once(':').ok_or_else(|| {
            HttpError::new(400, CODE_INVALID_REQUEST, "malformed request header line")
        })?;
        let name = name.trim();
        if name.is_empty() || !name.bytes().all(is_header_name_byte) {
            return Err(HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                "invalid request header name",
            ));
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(HttpError::new(
                431,
                CODE_PAYLOAD_TOO_LARGE,
                "too many request headers",
            ));
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }

    let header_value = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(existing, _): &&(String, String)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };

    if header_value("Transfer-Encoding").is_some() {
        return Err(HttpError::new(
            400,
            CODE_UNSUPPORTED_VALUE,
            "Transfer-Encoding is not supported; send Content-Length",
        ));
    }
    if let Some(expect) = header_value("Expect") {
        if expect.eq_ignore_ascii_case("100-continue") {
            return Err(HttpError::new(
                417,
                CODE_UNSUPPORTED_VALUE,
                "Expect: 100-continue is not supported by the KHAEN web entry",
            ));
        }
    }
    let content_length = match header_value("Content-Length") {
        Some(text) => parse_content_length(&text)?,
        None => 0usize,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(HttpError::new(
            413,
            CODE_PAYLOAD_TOO_LARGE,
            "request body exceeds the configured limit",
        ));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).map_err(|error| {
            HttpError::new(
                400,
                CODE_INVALID_REQUEST,
                format!("request body is shorter than Content-Length ({error})"),
            )
        })?;
    }

    let mut keep_alive = version == "HTTP/1.1";
    if let Some(connection) = header_value("Connection") {
        for token in connection.split(',') {
            let token = token.trim();
            if token.eq_ignore_ascii_case("close") {
                keep_alive = false;
            } else if token.eq_ignore_ascii_case("keep-alive") {
                keep_alive = true;
            }
        }
    }
    Ok(Some(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
        keep_alive,
        version: version.to_string(),
    }))
}

/// 读一行（含行尾；EOF 且无字节返回 0）。行长度超过头上限仍返回（由调用方统一 431）。
fn read_line<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> Result<usize, HttpError> {
    let read = reader.read_until(b'\n', out).map_err(|error| {
        HttpError::new(
            400,
            CODE_INVALID_REQUEST,
            format!("failed to read request head ({error})"),
        )
    })?;
    Ok(read)
}

/// 请求头名字节（RFC 7230 tchar）。
fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'|'
        )
}

/// 严格 Content-Length 解析（纯十进制；重复/矛盾/溢出拒绝）。
fn parse_content_length(text: &str) -> Result<usize, HttpError> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HttpError::new(
            400,
            CODE_INVALID_REQUEST,
            "invalid Content-Length",
        ));
    }
    trimmed
        .parse::<usize>()
        .map_err(|_| HttpError::new(400, CODE_INVALID_REQUEST, "Content-Length out of range"))
}

// ---------------------------------------------------------------------------
// HTTP/1.1 响应写出
// ---------------------------------------------------------------------------

/// 状态码原因短语（R50/API.md 状态集合）。
pub fn status_text(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        417 => "Expectation Failed",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Status",
    }
}

/// 写出 HTTP 响应（含 Content-Length/Connection）。
pub fn write_http_response<W: Write>(
    writer: &mut W,
    response: &HttpResponse,
    keep_alive: bool,
) -> io::Result<()> {
    let mut head = String::new();
    head.push_str("HTTP/1.1 ");
    head.push_str(&response.status.to_string());
    head.push(' ');
    head.push_str(status_text(response.status));
    head.push_str("\r\n");
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if response.header_value("Content-Length").is_none() {
        head.push_str("Content-Length: ");
        head.push_str(&response.body.len().to_string());
        head.push_str("\r\n");
    }
    head.push_str("Connection: ");
    head.push_str(if keep_alive { "keep-alive" } else { "close" });
    head.push_str("\r\n\r\n");
    writer.write_all(head.as_bytes())?;
    if !response.body.is_empty() {
        writer.write_all(&response.body)?;
    }
    writer.flush()
}

/// 静态资源响应（R51：UI 安全头 + 无 store）。
fn static_response(content_type: &str, body: &'static str) -> HttpResponse {
    let mut response = HttpResponse::bytes(200, content_type, body.as_bytes().to_vec());
    for (name, value) in UI_SECURITY_HEADERS {
        response = response.with_header(name, value);
    }
    response = response.with_header("Cache-Control", "no-store");
    response = response.with_header("X-KHAEN-Web-Version", WEB_VERSION);
    response
}

// ---------------------------------------------------------------------------
// 内嵌页面资源（R51：/、/index.html、/style.css、/app.js、/favicon.*）
// 全部内嵌、同源；无外部 CDN/字体/图片；无 inline script/style/on* 属性（CSP script-src self）。
// ---------------------------------------------------------------------------

/// 首页 HTML（真实控制台：状态/模式/音量/路由/格式/校准/WS 全交互）。
pub const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>KHAEN RT-first DSP 控制台</title>
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="stylesheet" href="/style.css">
</head>
<body>
<header class="topbar">
  <div class="brand"><span class="logo">KHAEN</span><span class="sub">RT-first DSP 控制台</span></div>
  <div class="badges">
    <span id="ws-badge" class="badge off">WS 未连接</span>
    <span id="auth-badge" class="badge off">未登录</span>
    <span id="build-badge" class="badge">web v?</span>
  </div>
</header>

<main id="login-view" class="view">
  <section class="card narrow">
    <h2>登录 / 初始化</h2>
    <p id="login-hint" class="hint">首次使用请设置管理员密码；也可用 REST API token 做会话验证（token 模式不含 WS Cookie 会话）。</p>
    <div class="grid">
      <label>管理员密码 <input id="login-password" type="password" autocomplete="current-password"></label>
      <label id="login-confirm-row" class="hidden">确认密码 <input id="login-confirm" type="password" autocomplete="new-password"></label>
      <label>API token（可选，脚本/调试） <input id="login-token" type="password" autocomplete="off" spellcheck="false"></label>
    </div>
    <div class="actions">
      <button id="btn-setup" data-action="setup" class="primary hidden">初始化管理员密码</button>
      <button id="btn-login" data-action="login" class="primary">密码登录</button>
      <button id="btn-token" data-action="token-login">用 token 验证</button>
      <button id="btn-logout" data-action="logout">退出或不使用 token</button>
    </div>
    <p id="login-error" class="error-line"></p>
    <p class="hint mono">公开探针：<a href="/healthz">/healthz</a> · WS：/ws（需同一套登录会话）</p>
  </section>
</main>

<main id="app-view" class="view hidden">
  <p id="app-error" class="error-line"></p>

  <section class="card wide">
    <h2>运行状态 <small class="hint">actual 值来自自有 REST（T42）；synthetic 回执必须显式标注，不假 ack</small></h2>
    <div class="kv">
      <div><span>运行时</span><strong id="st-runtime">-</strong></div>
      <div><span>engine / 真实性</span><strong id="st-engine">-</strong></div>
      <div><span>模式</span><strong id="st-mode">-</strong></div>
      <div><span>采样率 / chunksize</span><strong id="st-rate">-</strong></div>
      <div><span>设备 owner</span><strong id="st-owner">-</strong></div>
      <div><span>actual version</span><strong id="st-version">-</strong></div>
      <div><span>epoch</span><strong id="st-epoch">-</strong></div>
      <div><span>graph_id</span><strong id="st-graph" class="mono">-</strong></div>
      <div><span>target hash</span><strong id="st-target-hash" class="mono">-</strong></div>
      <div><span>xrun / deadline / clip</span><strong id="st-counters">-</strong></div>
      <div><span>音量 / mute</span><strong id="st-volume">-</strong></div>
      <div><span>pending prepare</span><strong id="st-pending">-</strong></div>
      <div><span>校准独占设备</span><strong id="st-cal-owner">-</strong></div>
    </div>
    <div class="actions">
      <button data-action="refresh" class="primary">刷新状态</button>
      <span class="hint">模式切换（唯一 owner；202 接受后轮询 actual）：</span>
      <button data-action="mode" data-mode="off">off</button>
      <button data-action="mode" data-mode="toslink">toslink</button>
      <button data-action="mode" data-mode="target_multichannel">target_multichannel</button>
      <button data-action="mode" data-mode="aes67">aes67</button>
      <button data-action="mode" data-mode="dante">dante</button>
      <span id="mode-result" class="hint"></span>
    </div>
  </section>

  <section class="card">
    <h2>音量 / 静音 <small class="hint">-150.0 .. +50.0 dB，mute 独立</small></h2>
    <div class="row">
      <input id="volume-slider" type="range" min="-150" max="50" step="0.5" value="-12">
      <output id="volume-output">-12.0 dB</output>
    </div>
    <div class="actions">
      <button data-action="volume-apply" class="primary">应用音量（回读；synthetic 会标注）</button>
      <button data-action="mute-toggle">静音切换</button>
      <span id="mute-state" class="hint">mute = false</span>
      <span id="volume-result" class="hint"></span>
    </div>
  </section>

  <section class="card wide">
    <h2>输入 / 路由 <small class="hint">role -> DAC 由 target 显式给出；编辑后 prepare->commit 实际生效</small></h2>
    <p class="hint">P1 4.1.2 唯一映射（overlay layout-presets，T57 解析）：FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，ch6 空；低带 dense 0..4 -> DAC 0/1/2/3/7 另表。禁止 SUB4。</p>
    <table class="routing">
      <thead><tr><th>输出 role</th><th>DAC 通道</th></tr></thead>
      <tbody id="routing-body"></tbody>
    </table>
    <div class="actions">
      <label>layout <input id="target-layout" type="text" value="4.1.2" spellcheck="false"></label>
      <label>profile <input id="target-profile" type="text" value="p1-412" spellcheck="false"></label>
      <label>channels <input id="target-channels" type="number" min="2" max="8" step="1" value="8"></label>
      <button data-action="routing-apply" class="primary">应用路由（PUT target + prepare + commit）</button>
      <button data-action="routing-reload">恢复 actual 路由</button>
    </div>
    <p id="routing-result" class="hint"></p>
  </section>

  <section class="card">
    <h2>音频格式 <small class="hint">runtime 仅 64/128 chunksize；其它值显式 400</small></h2>
    <div class="kv">
      <div><span>回读（actual / synthetic）</span><strong id="fmt-actual">-</strong></div>
      <div><span>允许 sample_rate</span><strong id="fmt-rates">-</strong></div>
      <div><span>允许 chunksize</span><strong id="fmt-chunks">-</strong></div>
    </div>
    <div class="actions">
      <label>sample_rate <select id="fmt-rate"></select></label>
      <label>chunksize <select id="fmt-chunk"></select></label>
      <button data-action="format-apply" class="primary">应用格式（prepare+commit）</button>
      <span id="format-result" class="hint"></span>
    </div>
  </section>

  <section class="card wide">
    <h2>校准 <small class="hint">T60 daemon 状态机 / T66 apply·retest·rollback 回执</small></h2>
    <p class="hint">apply 成功只表示实际生效（actual version 回读）；复测实测通过前为 APPLIED_NOT_CONFIRMED。rollback 回自研 last-good 图/系数，不回 CamillaDSP。apply 需要 job 到 prepared；retest 需要已 applied。离线 synthetic 模式（默认）下 engine=t60-jobmanager-synthetic：T60 状态机真实执行，但测量/apply ack 为合成模型，页面显式标 synthetic、不得当板端 actual。</p>
    <p class="hint">完整校准向导（麦克风/校准设置 → Level 对齐 → Delay 对齐 → Bass management → 皇帝位）：<a href="/calibration">打开 /calibration</a>。level/delay/bass 的 host 字段未落地时 REST 显式 501 pending_host_field，页面不会伪造 applied。</p>
    <div class="actions">
      <button data-action="cal-start-full" class="primary">启动完整校准（full）</button>
      <button data-action="cal-start-retest">启动复测校准（retest）</button>
      <button data-action="cal-start-level">启动 Level 对齐（stage=level）</button>
      <button data-action="cal-start-delay">启动 Delay 对齐（stage=delay）</button>
      <label>label <input id="cal-label" type="text" placeholder="可选" spellcheck="false"></label>
      <span id="cal-result" class="hint"></span>
      <span id="cal-engine" class="hint"></span>
    </div>
    <table class="jobs">
      <thead><tr><th>job</th><th>kind</th><th>state / stage</th><th>progress</th><th>applied_version</th><th>last_good</th><th>failure</th><th>操作</th></tr></thead>
      <tbody id="cal-body"></tbody>
    </table>
  </section>

  <section class="card wide">
    <h2>WS 实时状态 <small class="hint">/ws 文本 JSON；T24 兼容子集 + GetCalibrationJobs 扩展</small></h2>
    <div class="actions">
      <button data-action="ws-connect" class="primary">连接 WS</button>
      <button data-action="ws-disconnect">断开 WS</button>
      <label>轮询间隔 ms <input id="ws-interval" type="number" min="500" max="10000" step="500" value="2000"></label>
      <span id="ws-state-badge" class="hint">未连接</span>
    </div>
    <div class="kv">
      <div><span>GetState</span><strong id="ws-state">-</strong></div>
      <div><span>GetVolume / GetMute</span><strong id="ws-volume">-</strong></div>
      <div><span>GetCaptureRate</span><strong id="ws-rate">-</strong></div>
      <div><span>GetClippedSamples</span><strong id="ws-clip">-</strong></div>
      <div><span>GetConfigVersion</span><strong id="ws-version" class="mono">-</strong></div>
      <div><span>GetCalibrationJobs</span><strong id="ws-cal">-</strong></div>
    </div>
    <pre id="ws-log" class="console" aria-live="polite"></pre>
  </section>
</main>

<footer class="footer">
  <span>KHAEN RT-first DSP · web v?</span>
  <span class="hint">REST 契约见 /api/runtime/status；WS 命令见 T24 兼容矩阵；本页面无外部资源。</span>
</footer>
<noscript class="error-line">本控制台需要启用 JavaScript。</noscript>
<script src="/app.js" defer></script>
</body>
</html>
"##;

/// 样式表（无外部资源；深色控制台）。
pub const STYLE_CSS: &str = r##":root {
  color-scheme: dark;
  --bg: #0b1020;
  --panel: #121a2f;
  --panel-2: #18233d;
  --line: #26324f;
  --text: #e6edf7;
  --muted: #93a4c4;
  --accent: #47b3ff;
  --ok: #3ddc97;
  --warn: #ffcc66;
  --err: #ff6b81;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  font: 14px/1.5 "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
  background: var(--bg);
  color: var(--text);
}
a { color: var(--accent); }
.topbar {
  display: flex; align-items: center; justify-content: space-between;
  padding: 10px 18px; border-bottom: 1px solid var(--line); background: #0e1526;
  position: sticky; top: 0; z-index: 5;
}
.brand { display: flex; align-items: baseline; gap: 10px; }
.logo { font-weight: 700; letter-spacing: 2px; color: var(--accent); }
.sub { color: var(--muted); font-size: 12px; }
.badges { display: flex; gap: 8px; }
.badge { border: 1px solid var(--line); border-radius: 999px; padding: 2px 10px; font-size: 12px; color: var(--muted); }
.badge.on { color: var(--ok); border-color: var(--ok); }
.badge.off { color: var(--muted); }
.badge.warn { color: var(--warn); border-color: var(--warn); }
.view { max-width: 1180px; margin: 0 auto; padding: 16px; display: grid; gap: 14px; }
.view.hidden { display: none; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 14px 16px; }
.card.wide { grid-column: 1 / -1; }
.card.narrow { max-width: 640px; margin-left: auto; margin-right: auto; }
.card h2 { margin: 0 0 10px; font-size: 15px; font-weight: 600; }
.card h2 small { font-weight: 400; margin-left: 8px; }
.hint { color: var(--muted); font-size: 12px; }
.mono { font-family: "Cascadia Mono", "Consolas", monospace; font-size: 12px; word-break: break-all; }
.grid { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); }
.kv { display: grid; gap: 8px; grid-template-columns: repeat(auto-fit, minmax(230px, 1fr)); }
.kv > div { background: var(--panel-2); border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; }
.kv span { display: block; color: var(--muted); font-size: 11px; }
.kv strong { font-weight: 600; }
.row { display: flex; align-items: center; gap: 12px; }
input, select, button {
  font: inherit; color: var(--text); background: #0f1830; border: 1px solid var(--line);
  border-radius: 7px; padding: 6px 9px;
}
input[type="range"] { width: 320px; padding: 0; }
button { cursor: pointer; }
button:hover { border-color: var(--accent); }
button.primary { background: #153a5c; border-color: #2b6f9e; }
button:disabled { opacity: 0.45; cursor: not-allowed; }
label { display: grid; gap: 4px; color: var(--muted); font-size: 12px; }
.actions { display: flex; flex-wrap: wrap; align-items: end; gap: 8px; margin-top: 10px; }
output { font-family: "Cascadia Mono", "Consolas", monospace; min-width: 90px; display: inline-block; }
table { width: 100%; border-collapse: collapse; margin-top: 10px; font-size: 13px; }
th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--line); vertical-align: top; }
th { color: var(--muted); font-weight: 500; font-size: 12px; }
td.actions-cell { white-space: nowrap; }
td.actions-cell button { padding: 3px 8px; font-size: 12px; margin-right: 4px; }
.progress { height: 8px; background: #0f1830; border: 1px solid var(--line); border-radius: 999px; overflow: hidden; min-width: 80px; }
.progress > i { display: block; height: 100%; background: var(--accent); width: 0; }
.state-ok { color: var(--ok); }
.state-err { color: var(--err); }
.state-warn { color: var(--warn); }
.error-line { color: var(--err); min-height: 18px; margin: 8px 0 0; }
.console {
  margin: 10px 0 0; padding: 10px; background: #070c18; border: 1px solid var(--line);
  border-radius: 8px; max-height: 220px; overflow: auto; font-size: 12px;
  font-family: "Cascadia Mono", "Consolas", monospace; white-space: pre-wrap; word-break: break-all;
}
.footer { max-width: 1180px; margin: 0 auto; padding: 10px 16px 28px; display: flex; gap: 14px; color: var(--muted); font-size: 12px; }
.hidden { display: none !important; }
@media (min-width: 860px) {
  .view { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
"##;

/// 页面脚本（原生 JS；无外部依赖；无 inline 事件）。
pub const APP_JS: &str = r##""use strict";
// KHAEN RT-first DSP 控制台（WBS T70）。
// 数据面：REST /api/*（T42，唯一 actual ack 来源）+ WS /ws（T24 兼容子集 + GetCalibrationJobs）。
// 本文件无外部依赖、无动态资源；所有按钮通过 data-action 绑定（CSP: script-src self）。
(function () {
  var ROLES = ["FL", "FR", "C", "SL", "SR", "SUB1", "SUB2", "SUB3", "HFL", "HFR"];
  var PATHS = {
    authStatus: "/api/auth/status",
    authSetup: "/api/auth/setup",
    authLogin: "/api/auth/login",
    authSession: "/api/auth/session",
    authLogout: "/api/auth/logout",
    status: "/api/runtime/status",
    runtimeMode: "/api/runtime/mode",
    volume: "/api/runtime/volume",
    mute: "/api/runtime/mute",
    target: "/api/runtime/target",
    prepare: "/api/runtime/prepare",
    commit: "/api/runtime/commit",
    format: "/api/audio/format",
    jobs: "/api/calibration/jobs",
    calibration: "/calibration",
    ws: "/ws"
  };
  var state = {
    token: window.localStorage.getItem("khaen.token") || "",
    csrf: null,
    authKind: null,
    authenticated: false,
    muted: false,
    volumeDb: -12,
    polling: null,
    ws: null,
    wsTimer: null,
    wsQueue: [],
    wsSending: false,
    lastJobs: [],
    calSynthetic: false
  };

  function byId(id) { return document.getElementById(id); }
  function setText(id, value) { var node = byId(id); if (node) { node.textContent = value; } }
  // 校准 job 的 engine/synthetic 事实（REST 原样带出；页面不得把离线合成回执显示成 actual）。
  function calSynthetic() { return state.calSynthetic === true; }
  function calMark() {
    return calSynthetic() ? " · synthetic（离线状态机/合成测量与 ack，非板端 actual）" : "";
  }
  function setCalEngine(payload) {
    var engine = payload && payload.engine ? payload.engine : "";
    state.calSynthetic = !!(payload && (payload.synthetic === true || (payload.cal_engine && payload.cal_engine.synthetic === true)));
    var node = byId("cal-engine");
    if (!node) { return; }
    if (!engine) { node.textContent = ""; return; }
    node.textContent = "engine=" + engine + (state.calSynthetic
      ? " · synthetic（离线状态机/合成测量与 ack；非板端 actual）"
      : " · actual");
  }
  function show(id, visible) { var node = byId(id); if (node) { node.classList.toggle("hidden", !visible); } }
  function compact(value) {
    if (value === null || value === undefined) { return "-"; }
    if (typeof value === "boolean") { return value ? "true" : "false"; }
    if (typeof value === "number") { return String(value); }
    if (typeof value === "string") { return value; }
    return JSON.stringify(value);
  }
  function shortHash(text) {
    if (!text || typeof text !== "string") { return "-"; }
    return text.length > 20 ? text.slice(0, 16) + "..." : text;
  }
  function errorText(error) {
    if (!error) { return "unknown error"; }
    if (error.body && error.body.error) {
      return error.body.error.code + ": " + error.body.error.message;
    }
    if (typeof error.body === "string") { return error.status + ": " + error.body; }
    if (error.message) { return error.message; }
    return String(error.status || error);
  }
  function setError(id, error) {
    var node = byId(id);
    if (!node) { return; }
    node.textContent = error ? errorText(error) : "";
  }
  function logLine(text) {
    var node = byId("ws-log");
    if (!node) { return; }
    var stamp = new Date().toLocaleTimeString();
    var lines = node.textContent ? node.textContent.split("\n") : [];
    lines.push("[" + stamp + "] " + text);
    while (lines.length > 40) { lines.shift(); }
    node.textContent = lines.join("\n");
    node.scrollTop = node.scrollHeight;
  }

  function api(method, path, body) {
    var headers = {};
    if (state.token) { headers["X-KHAEN-Token"] = state.token; }
    if (body !== undefined && body !== null) { headers["Content-Type"] = "application/json"; }
    if (method !== "GET" && state.csrf) { headers["X-KHAEN-CSRF"] = state.csrf; }
    var options = { method: method, headers: headers, credentials: "same-origin" };
    if (body !== undefined && body !== null) { options.body = JSON.stringify(body); }
    return fetch(path, options).then(function (response) {
      return response.text().then(function (text) {
        var json = null;
        if (text) {
          try { json = JSON.parse(text); } catch (error) { json = null; }
        }
        if (!response.ok) {
          throw { status: response.status, body: json !== null ? json : text };
        }
        return json;
      });
    });
  }

  function rememberSession(payload) {
    if (payload && typeof payload.csrf_token === "string" && payload.csrf_token) {
      state.csrf = payload.csrf_token;
    }
    if (payload && typeof payload.auth_kind === "string") {
      state.authKind = payload.auth_kind;
    }
  }

  function setAuthenticated(flag) {
    state.authenticated = flag;
    var badge = byId("auth-badge");
    if (badge) {
      badge.textContent = flag ? "已登录" : "未登录";
      badge.className = "badge " + (flag ? "on" : "off");
    }
    show("app-view", flag);
    show("login-view", !flag);
    if (flag) {
      startPolling();
      refreshAll();
    } else {
      stopPolling();
      disconnectWs();
    }
  }

  function refreshAuthStatus() {
    return api("GET", PATHS.authStatus).then(function (status) {
      var setup = !!status.setup_required;
      show("btn-setup", setup);
      show("login-confirm-row", setup);
      show("btn-login", !setup);
      var hint = byId("login-hint");
      if (hint) {
        hint.textContent = setup
          ? "首次使用：请设置管理员密码（>= 8 位）后自动登录。"
          : "输入管理员密码登录；或用 REST API token 做会话验证。";
      }
      var build = byId("build-badge");
      if (build) { build.textContent = "web " + (status.mock ? "mock" : "live"); }
      return status;
    });
  }

  function verifySession() {
    return api("GET", PATHS.authSession).then(function (session) {
      rememberSession(session);
      setAuthenticated(true);
      return session;
    });
  }

  function loginWithPassword() {
    var password = byId("login-password").value;
    setError("login-error", null);
    return api("POST", PATHS.authLogin, { password: password }).then(function (payload) {
      rememberSession(payload);
      return verifySession();
    }).catch(function (error) {
      setError("login-error", error);
      setAuthenticated(false);
    });
  }

  function setupPassword() {
    var password = byId("login-password").value;
    var confirm = byId("login-confirm").value;
    setError("login-error", null);
    return api("POST", PATHS.authSetup, { password: password, confirm: confirm }).then(function (payload) {
      rememberSession(payload);
      return verifySession();
    }).catch(function (error) {
      setError("login-error", error);
    });
  }

  function tokenLogin() {
    var token = byId("login-token").value.trim();
    if (!token) { setError("login-error", { message: "请输入 token" }); return; }
    state.token = token;
    window.localStorage.setItem("khaen.token", token);
    setError("login-error", null);
    verifySession().catch(function (error) {
      setError("login-error", error);
      setAuthenticated(false);
    });
  }

  function logout() {
    api("POST", PATHS.authLogout).catch(function () { return null; }).then(function () {
      state.token = "";
      state.csrf = null;
      window.localStorage.removeItem("khaen.token");
      setAuthenticated(false);
      refreshAuthStatus();
    });
  }

  function isSynthetic(payload) {
    return !!(payload && payload.synthetic === true);
  }

  function engineText(status) {
    var engine = compact(status.engine || "unknown");
    if (isSynthetic(status)) {
      return engine + "（synthetic：内存 engine 未接真实 host，非 actual）";
    }
    return engine + "（actual host engine）";
  }

  function ackText(payload) {
    return isSynthetic(payload) ? "synthetic（未接真实 host，非 actual）" : "actual";
  }

  function renderStatus(status) {
    setText("st-runtime", compact(status.runtime_state));
    setText("st-engine", engineText(status));
    setText("st-mode", compact(status.mode) + " (" + compact(status.mode_label) + ")");
    setText("st-rate", compact(status.sample_rate) + " Hz / " + compact(status.chunksize) + " frames");
    setText("st-owner", compact(status.device_owner));
    setText("st-version", compact(status.actual_version) + (isSynthetic(status) ? "（synthetic）" : ""));
    setText("st-epoch", compact(status.epoch));
    setText("st-graph", shortHash(status.graph_id));
    setText("st-target-hash", shortHash(status.target_hash));
    setText("st-counters", compact(status.xruns) + " / " + compact(status.deadline_misses) + " / " + compact(status.clipped_samples));
    state.muted = !!status.mute;
    if (typeof status.volume_db === "number") { state.volumeDb = status.volume_db; }
    setText("st-volume", compact(status.volume_db) + " dB / mute=" + compact(status.mute));
    setText("st-pending", status.pending_prepare ? compact(status.pending_prepare) : "无");
    setText("st-cal-owner", compact(status.calibration_owner));
    var slider = byId("volume-slider");
    if (slider && typeof status.volume_db === "number") { slider.value = String(status.volume_db); }
    var output = byId("volume-output");
    if (output && typeof status.volume_db === "number") { output.textContent = status.volume_db.toFixed(1) + " dB"; }
    setText("mute-state", "mute = " + compact(status.mute));
    var target = status.target;
    if (target) {
      setText("target-layout", compact(target.layout));
      if (target.layout) { byId("target-layout").value = target.layout; }
      if (target.profile) { byId("target-profile").value = target.profile; }
      if (target.channels) { byId("target-channels").value = String(target.channels); }
      renderRouting(target);
    }
  }

  function refreshStatus() {
    return api("GET", PATHS.status).then(function (status) {
      renderStatus(status);
      setError("app-error", null);
      return status;
    }).catch(function (error) { setError("app-error", error); });
  }

  function switchMode(mode) {
    setError("app-error", null);
    setText("mode-result", "切换中: " + mode);
    return api("PUT", PATHS.runtimeMode, { mode: mode }).then(function (payload) {
      var stateText = payload && payload.switch_state ? payload.switch_state : "accepted";
      var evidence = (payload && payload.mode_evidence) || null;
      var controller = !!(payload && payload.mode_controller === true);
      var modeActual = !!(payload && payload.mode_actual === true);
      var services = compact(payload && payload.mode_services);
      setText("mode-result", "202 接受（" + stateText + "）；" + (controller ? ("T44 状态机 / 服务 " + services) : "synthetic：模式状态机未接线") + "，等待 actual");
      var tries = 0;
      function poll() {
        tries += 1;
        return refreshStatus().then(function (status) {
          if (status && status.mode === mode) {
            var prefix = modeActual
              ? "已生效: "
              : (controller
                ? "已切换（T44 状态机；服务 " + services + "，非板端 actual）: "
                : "模式已变（synthetic 内存 engine，非 actual）: ");
            var marker = evidence ? (" epoch=" + compact(evidence.epoch) + " servo=" + compact(evidence.servo_owner)) : "";
            setText("mode-result", prefix + mode + " / " + compact(status.mode_profile) + marker);
            return null;
          }
          if (tries >= 10) { setText("mode-result", "已接受但 actual 仍为 " + compact(status && status.mode)); return null; }
          return new Promise(function (resolve) { window.setTimeout(resolve, 300); }).then(poll);
        });
      }
      return poll();
    }).catch(function (error) {
      setError("app-error", error);
      setText("mode-result", "切换失败");
    });
  }

  function applyVolume() {
    var slider = byId("volume-slider");
    var value = Number(slider.value);
    setError("app-error", null);
    return api("PUT", PATHS.volume, { db: value }).then(function (payload) {
      var ack = ackText(payload);
      setText("volume-result", ack + " " + compact(payload.volume_db) + " dB / mute=" + compact(payload.mute) + " / actual_version=" + compact(payload.actual_version));
      return refreshStatus();
    }).catch(function (error) { setError("app-error", error); });
  }

  function toggleMute() {
    setError("app-error", null);
    return api("PUT", PATHS.mute, { mute: !state.muted }).then(function (payload) {
      state.muted = !!payload.mute;
      var ack = ackText(payload);
      setText("mute-state", "mute = " + compact(payload.mute) + "（" + ack + "）");
      setText("volume-result", ack + " mute=" + compact(payload.mute) + " / volume=" + compact(payload.volume_db) + " dB");
      return refreshStatus();
    }).catch(function (error) { setError("app-error", error); });
  }

  function renderRouting(target) {
    var body = byId("routing-body");
    if (!body) { return; }
    body.textContent = "";
    var roles = (target && target.roles) || [];
    roles.forEach(function (entry) {
      var tr = document.createElement("tr");
      var tdRole = document.createElement("td");
      tdRole.textContent = entry.role;
      var tdDac = document.createElement("td");
      var select = document.createElement("select");
      var channels = (target && target.channels) || 8;
      for (var dac = 0; dac < channels; dac += 1) {
        var option = document.createElement("option");
        option.value = String(dac);
        option.textContent = String(dac);
        if (dac === entry.dac) { option.selected = true; }
        select.appendChild(option);
      }
      select.setAttribute("data-role", entry.role);
      select.className = "dac-select";
      tdDac.appendChild(select);
      tr.appendChild(tdRole);
      tr.appendChild(tdDac);
      body.appendChild(tr);
    });
  }

  function readRoutingFromTable() {
    var roles = [];
    var selects = document.querySelectorAll("#routing-body select.dac-select");
    for (var index = 0; index < selects.length; index += 1) {
      var select = selects[index];
      roles.push({ role: select.getAttribute("data-role"), dac: Number(select.value) });
    }
    return roles;
  }

  function applyRouting() {
    setError("app-error", null);
    var target = {
      layout: byId("target-layout").value.trim(),
      profile: byId("target-profile").value.trim(),
      channels: Number(byId("target-channels").value),
      roles: readRoutingFromTable(),
      low_band_dacs: [0, 1, 2, 3, 7]
    };
    return api("PUT", PATHS.target, { target: target }).then(function () {
      return api("POST", PATHS.prepare, {});
    }).then(function (prepared) {
      return api("POST", PATHS.commit, { prepare_id: prepared.prepare_id }).then(function (commit) {
        var prefix = isSynthetic(commit)
          ? "synthetic 回执（内存 engine 未接真实 host，非 actual）: version="
          : "已生效（actual）: version=";
        setText("routing-result", prefix + compact(commit.actual_version) + " graph=" + shortHash(commit.graph_id) + " epoch=" + compact(commit.epoch));
        return refreshStatus();
      });
    }).catch(function (error) {
      setError("app-error", error);
      setText("routing-result", "路由应用失败（未生效）");
    });
  }

  function refreshRouting() {
    return api("GET", PATHS.target).then(function (payload) {
      if (payload && payload.target) { renderRouting(payload.target); }
    }).catch(function (error) { setError("app-error", error); });
  }

  function refreshFormat() {
    return api("GET", PATHS.format).then(function (format) {
      var fmtAck = isSynthetic(format) ? " / synthetic（非 actual）" : " / actual";
      setText("fmt-actual", compact(format.input_format) + " / chunksize=" + compact(format.chunksize) + fmtAck);
      var rates = (format.limits && format.limits.sample_rates) || [48000, 96000];
      var chunks = (format.limits && format.limits.chunksizes) || [64, 128];
      setText("fmt-rates", rates.join(" / "));
      setText("fmt-chunks", chunks.join(" / "));
      var rateSelect = byId("fmt-rate");
      var chunkSelect = byId("fmt-chunk");
      rateSelect.textContent = "";
      chunkSelect.textContent = "";
      rates.forEach(function (rate) {
        var option = document.createElement("option");
        option.value = String(rate);
        option.textContent = String(rate);
        if (rate === format.sample_rate) { option.selected = true; }
        rateSelect.appendChild(option);
      });
      chunks.forEach(function (size) {
        var option = document.createElement("option");
        option.value = String(size);
        option.textContent = String(size);
        if (size === format.chunksize) { option.selected = true; }
        chunkSelect.appendChild(option);
      });
    }).catch(function (error) { setError("app-error", error); });
  }

  function applyFormat() {
    var body = {
      sample_rate: Number(byId("fmt-rate").value),
      chunksize: Number(byId("fmt-chunk").value)
    };
    setError("app-error", null);
    return api("PUT", PATHS.format, body).then(function (payload) {
      var outcome = payload.outcome || {};
      var prefix = isSynthetic(outcome) ? "synthetic 回执（非 actual）: version=" : "已生效（actual）: version=";
      setText("format-result", prefix + compact(outcome.actual_version) + " graph=" + shortHash(outcome.graph_id) + " epoch=" + compact(outcome.epoch));
      return refreshStatus();
    }).catch(function (error) {
      setError("app-error", error);
      setText("format-result", "格式应用失败（未生效）");
    });
  }

  function jobRow(job) {
    var tr = document.createElement("tr");
    function cell(text, className) {
      var td = document.createElement("td");
      if (className) { td.className = className; }
      td.textContent = text;
      return td;
    }
    tr.appendChild(cell(job.job_id));
    tr.appendChild(cell(job.kind));
    var stateText = job.state + (job.stage ? " / " + job.stage : "");
    var stateCell = cell(stateText);
    if (job.state === "failed") { stateCell.className = "state-err"; }
    if (job.state === "complete") { stateCell.className = "state-ok"; }
    if (job.state === "rolled_back" || job.state === "cancelled") { stateCell.className = "state-warn"; }
    tr.appendChild(stateCell);
    var progressCell = document.createElement("td");
    var progress = document.createElement("div");
    progress.className = "progress";
    var bar = document.createElement("i");
    var ratio = typeof job.progress === "number" ? Math.max(0, Math.min(1, job.progress)) : 0;
    bar.style.width = String(Math.round(ratio * 100)) + "%";
    progress.appendChild(bar);
    progressCell.appendChild(progress);
    if (job.progress_text) {
      var progressText = document.createElement("div");
      progressText.className = "hint";
      progressText.textContent = job.progress_text;
      progressCell.appendChild(progressText);
    }
    if (job.lanes && job.lanes.lanes && job.lanes.lanes.length) {
      var laneText = document.createElement("div");
      laneText.className = "hint";
      laneText.textContent = "lanes " + job.lanes.locked + "/" + job.lanes.total + " locked";
      progressCell.appendChild(laneText);
    }
    tr.appendChild(progressCell);
    tr.appendChild(cell(job.applied_version || "-"));
    tr.appendChild(cell(job.last_good_version || "-"));
    tr.appendChild(cell(job.failure || "-"));
    var actionsCell = document.createElement("td");
    actionsCell.className = "actions-cell";
    var terminal = job.state === "complete" || job.state === "failed" || job.state === "cancelled" || job.state === "rolled_back";
    function actionButton(label, action, enabled) {
      var button = document.createElement("button");
      button.textContent = label;
      button.setAttribute("data-action", action);
      button.setAttribute("data-job", job.job_id);
      button.disabled = !enabled;
      actionsCell.appendChild(button);
    }
    actionButton("取消", "cal-cancel", !terminal);
    actionButton("应用", "cal-apply", job.state === "prepared");
    actionButton("复测", "cal-retest", !!job.applied_version);
    actionButton("回滚", "cal-rollback", !!job.last_good_version);
    tr.appendChild(actionsCell);
    return tr;
  }

  function renderJobs(jobs) {
    var body = byId("cal-body");
    if (!body) { return; }
    state.lastJobs = jobs || [];
    body.textContent = "";
    if (!state.lastJobs.length) {
      var tr = document.createElement("tr");
      var td = document.createElement("td");
      td.colSpan = 8;
      td.className = "hint";
      td.textContent = "暂无校准 job（点击上方按钮启动）";
      tr.appendChild(td);
      body.appendChild(tr);
      return;
    }
    state.lastJobs.forEach(function (job) { body.appendChild(jobRow(job)); });
  }

  function refreshJobs() {
    return api("GET", PATHS.jobs).then(function (payload) {
      setCalEngine(payload || null);
      renderJobs((payload && payload.jobs) || []);
      setError("cal-result", null);
    }).catch(function (error) {
      var body = byId("cal-body");
      if (body && (!error || !error.body || (error.body.error && error.body.error.code !== "service_unavailable"))) {
        setError("cal-result", error);
      } else if (error && error.body && error.body.error && error.body.error.code === "service_unavailable") {
        state.calSynthetic = false;
        setText("cal-engine", "engine=not_wired（校准 daemon 未接线；REST 显式 503，不伪造 job）");
        setError("cal-result", { message: "校准 daemon 未接线（" + error.body.error.message + "）" });
      }
    });
  }

  function startCalibration(kind, stage) {
    var label = byId("cal-label").value.trim();
    var body = { kind: kind };
    if (stage) { body.stage = stage; body.audible_sweep_authorized = true; }
    if (label) { body.label = label; }
    setError("app-error", null);
    return api("POST", PATHS.jobs, body).then(function (payload) {
      setText("cal-result", "已启动 " + compact(payload.job_id) + "（" + kind + (stage ? "/" + stage : "") + "）" + calMark());
      return refreshJobs();
    }).catch(function (error) {
      setError("app-error", error);
      setText("cal-result", "启动失败");
    });
  }

  function jobAction(action, jobId) {
    var suffix = action;
    setError("app-error", null);
    return api("POST", PATHS.jobs + "/" + encodeURIComponent(jobId) + "/" + suffix, {}).then(function (payload) {
      var detail = "";
      if (payload && payload.applied_version) { detail = " applied_version=" + payload.applied_version; }
      if (payload && payload.last_good_version) { detail = " last_good=" + payload.last_good_version; }
      setText("cal-result", action + " 成功: " + jobId + detail + calMark());
      return refreshJobs();
    }).catch(function (error) {
      setError("app-error", error);
      setText("cal-result", action + " 失败: " + jobId);
    });
  }

  function refreshAll() {
    refreshStatus();
    refreshRouting();
    refreshFormat();
    refreshJobs();
  }

  function startPolling() {
    stopPolling();
    state.polling = window.setInterval(function () {
      refreshStatus();
      refreshJobs();
    }, 2000);
  }

  function stopPolling() {
    if (state.polling) { window.clearInterval(state.polling); state.polling = null; }
  }

  function wsUrl() {
    var scheme = window.location.protocol === "https:" ? "wss://" : "ws://";
    var url = scheme + window.location.host + PATHS.ws;
    if (state.token) { url += "?token=" + encodeURIComponent(state.token); }
    return url;
  }

  function setWsBadge(text, kind) {
    var badge = byId("ws-badge");
    if (badge) { badge.textContent = text; badge.className = "badge " + (kind || "off"); }
    setText("ws-state-badge", text);
  }

  function connectWs() {
    disconnectWs();
    if (state.authKind === "token") {
      setWsBadge("token 模式无 Cookie 会话，请用密码登录后再连 WS", "warn");
      logLine("WS 需要 Cookie 会话或升级头 token；浏览器只在同源下发送 Cookie。");
      return;
    }
    var socket;
    try { socket = new WebSocket(wsUrl()); } catch (error) {
      setWsBadge("WS 创建失败", "warn"); logLine("ws error: " + error); return;
    }
    state.ws = socket;
    setWsBadge("WS 连接中", "warn");
    socket.onopen = function () {
      setWsBadge("WS 已连接", "on");
      logLine("open " + PATHS.ws);
      queueWs("GetVersion");
      queueWs("GetConfigVersion");
      startWsPolling();
    };
    socket.onmessage = function (event) {
      handleWsMessage(event.data);
    };
    socket.onerror = function () { logLine("ws transport error"); };
    socket.onclose = function (event) {
      state.ws = null;
      stopWsPolling();
      setWsBadge("WS 已断开 (" + event.code + ")", "off");
      logLine("close code=" + event.code + " reason=" + (event.reason || ""));
    };
  }

  function disconnectWs() {
    stopWsPolling();
    state.wsQueue = [];
    state.wsSending = false;
    if (state.ws) {
      try { state.ws.close(1000, "user_disconnect"); } catch (error) { /* ignore */ }
      state.ws = null;
    }
    setWsBadge("WS 未连接", "off");
  }

  function queueWs(command) {
    state.wsQueue.push(command);
    if (!state.wsSending) { sendNextWs(); }
  }

  function sendNextWs() {
    if (state.wsSending || !state.ws || state.ws.readyState !== 1) { return; }
    var command = state.wsQueue.shift();
    if (command === undefined) { return; }
    state.wsSending = true;
    state.ws.send(JSON.stringify(command));
  }

  function wsPayload(value) {
    if (value === undefined) { return null; }
    return value;
  }

  function handleWsMessage(raw) {
    state.wsSending = false;
    var message;
    try { message = JSON.parse(raw); } catch (error) {
      logLine("ws invalid json: " + raw); sendNextWs(); return;
    }
    var keys = Object.keys(message || {});
    if (!keys.length) { logLine("ws reply without key"); sendNextWs(); return; }
    var command = keys[0];
    var body = message[command] || {};
    if (body.result === "Ok") {
      applyWsValue(command, body.value);
      logLine(command + " -> Ok");
    } else {
      logLine(command + " -> " + JSON.stringify(body.result));
    }
    window.setTimeout(sendNextWs, 40);
  }

  function applyWsValue(command, value) {
    if (command === "GetState") { setText("ws-state", compact(value)); }
    if (command === "GetVolume") { setText("ws-volume", compact(value) + " dB"); }
    if (command === "GetMute") { setText("ws-volume", compact(state.volumeDb) + " dB / mute=" + compact(value)); }
    if (command === "GetCaptureRate") { setText("ws-rate", compact(value) + " Hz"); }
    if (command === "GetClippedSamples") { setText("ws-clip", compact(value)); }
    if (command === "GetConfigVersion") { setText("ws-version", shortHash(value && value.graph_id) + " v" + compact(value && value.plan_version)); }
    if (command === "GetCalibrationJobs") {
      var jobs = (value && value.jobs) || [];
      setText("ws-cal", jobs.length + " jobs / active=" + compact(findActiveJob(jobs)));
    }
    if (command === "GetVersion") { setText("build-badge", "ws " + compact(value)); }
  }

  function findActiveJob(jobs) {
    for (var index = 0; index < jobs.length; index += 1) {
      var job = jobs[index];
      var terminal = job.state === "complete" || job.state === "failed" || job.state === "cancelled" || job.state === "rolled_back";
      if (!terminal) { return job.job_id + ":" + job.state; }
    }
    return "none";
  }

  function startWsPolling() {
    stopWsPolling();
    var intervalNode = byId("ws-interval");
    var interval = intervalNode ? Number(intervalNode.value) : 2000;
    if (!(interval >= 500)) { interval = 2000; }
    state.wsTimer = window.setInterval(function () {
      if (!state.ws || state.ws.readyState !== 1) { return; }
      ["GetState", "GetVolume", "GetMute", "GetCaptureRate", "GetClippedSamples", "GetConfigVersion", "GetCalibrationJobs"].forEach(queueWs);
    }, interval);
  }

  function stopWsPolling() {
    if (state.wsTimer) { window.clearInterval(state.wsTimer); state.wsTimer = null; }
  }

  function onActionClick(event) {
    var target = event.target;
    if (!target || !target.getAttribute) { return; }
    var action = target.getAttribute("data-action");
    if (!action) { return; }
    var job = target.getAttribute("data-job");
    if (action === "login") { loginWithPassword(); }
    if (action === "setup") { setupPassword(); }
    if (action === "token-login") { tokenLogin(); }
    if (action === "logout") { logout(); }
    if (action === "refresh") { refreshAll(); }
    if (action === "mode") { switchMode(target.getAttribute("data-mode")); }
    if (action === "volume-apply") { applyVolume(); }
    if (action === "mute-toggle") { toggleMute(); }
    if (action === "routing-apply") { applyRouting(); }
    if (action === "routing-reload") { refreshRouting(); }
    if (action === "format-apply") { applyFormat(); }
    if (action === "cal-start-full") { startCalibration("full"); }
    if (action === "cal-start-retest") { startCalibration("retest"); }
    if (action === "cal-start-level") { startCalibration("emperor", "level"); }
    if (action === "cal-start-delay") { startCalibration("emperor", "delay"); }
    if (action === "cal-cancel" && job) { jobAction("cancel", job); }
    if (action === "cal-apply" && job) { jobAction("apply", job); }
    if (action === "cal-retest" && job) { jobAction("retest", job); }
    if (action === "cal-rollback" && job) { jobAction("rollback", job); }
    if (action === "ws-connect") { connectWs(); }
    if (action === "ws-disconnect") { disconnectWs(); }
  }

  function bindEvents() {
    document.addEventListener("click", onActionClick);
    var slider = byId("volume-slider");
    if (slider) {
      slider.addEventListener("input", function () {
        var output = byId("volume-output");
        if (output) { output.textContent = Number(slider.value).toFixed(1) + " dB"; }
      });
    }
  }

  function boot() {
    bindEvents();
    refreshAuthStatus().then(function () {
      return verifySession().catch(function () { return null; });
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
"##;

/// favicon（内嵌 SVG；无外部资源）。
pub const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="12" fill="#0b1020"/><path d="M14 22h36v6H36v22h-8V28H14z" fill="#47b3ff"/><circle cx="47" cy="47" r="7" fill="#3ddc97"/></svg>
"##;

/// 首页别名内容（与 INDEX_HTML 相同）。
pub fn index_html() -> &'static str {
    INDEX_HTML
}

/// 样式表内容。
pub fn style_css() -> &'static str {
    STYLE_CSS
}

/// 页面脚本内容。
pub fn app_js() -> &'static str {
    APP_JS
}

/// favicon 内容。
pub fn favicon_svg() -> &'static str {
    FAVICON_SVG
}

// ---------------------------------------------------------------------------
// 路由（R51/K01）：静态资源 | /api + /healthz -> T42 REST | /ws -> WebSocket
// ---------------------------------------------------------------------------

/// REST/健康探针路径（全部交给 RestService；含 /healthz 与 /api/*）。
pub fn is_rest_path(path: &str) -> bool {
    path == "/healthz" || path == "/api" || path.starts_with("/api/")
}

/// 内嵌 Vue 控制台静态资源响应（E-7）：静态表命中与 SPA 回退共用同一套安全头/缓存策略。
fn webui_static_response(asset: webui_assets::StaticAsset) -> HttpResponse {
    let mut response = HttpResponse::bytes(200, asset.content_type, asset.bytes.to_vec());
    for (name, value) in SPA_SECURITY_HEADERS {
        response = response.with_header(name, value);
    }
    // 文件名带内容 hash 的 /assets/* 长缓存；index.html（含回退）与无 hash 资源不缓存。
    response.with_header("Cache-Control", webui_assets::cache_control(asset.path))
}

/// 静态资源响应（R51 KEEP + E-7）。
///
/// 顺序：内嵌 Vue 控制台静态表 → 旧内嵌页/校准向导的精确路径 → SPA 回退 `index.html`。
/// 未命中的非 `/api` GET 一律回 index.html（Vue Router history 模式由前端接管未知路径）；
/// `/api/*` 与 `/ws` 在 `WebServer::handle` 里已先行分流，不会走到这里。
fn static_asset(path: &str) -> HttpResponse {
    if let Some(asset) = webui_assets::lookup(path) {
        return webui_static_response(asset);
    }
    match path {
        STYLE_CSS_PATH => static_response("text/css; charset=utf-8", STYLE_CSS),
        APP_JS_PATH => static_response("text/javascript; charset=utf-8", APP_JS),
        FAVICON_SVG_PATH | FAVICON_ICO_PATH => {
            static_response("image/svg+xml; charset=utf-8", FAVICON_SVG)
        }
        CALIBRATION_PAGE_PATH => static_response("text/html; charset=utf-8", CALIBRATION_HTML),
        CALIBRATION_CSS_PATH => static_response("text/css; charset=utf-8", CALIBRATION_CSS),
        CALIBRATION_JS_PATH => static_response("text/javascript; charset=utf-8", CALIBRATION_JS),
        _ => webui_static_response(webui_assets::index()),
    }
}

/// 静态资源方法校验（仅 GET；HEAD/OPTIONS 在解析阶段已 405，这里兜底）。
fn static_method_allowed(method: Method) -> bool {
    method == Method::Get
}

/// WS 握手：校验 Sec-WebSocket-Key/Version 并计算 Sec-WebSocket-Accept（RFC 6455 §4.2）。
pub fn ws_accept_key(client_key: &str) -> String {
    const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut material = String::with_capacity(client_key.len() + WS_GUID.len());
    material.push_str(client_key);
    material.push_str(WS_GUID);
    let digest = sha1(material.as_bytes());
    base64_encode(&digest)
}

/// Sec-WebSocket-Key 合法性（base64 解码后必须 16 字节；RFC 6455 §4.1）。
pub fn is_valid_ws_key(key: &str) -> bool {
    match base64_decode(key) {
        Some(bytes) => bytes.len() == 16,
        None => false,
    }
}

/// 是否为合法的 WS 版本（仅 13）。
fn is_valid_ws_version(request: &HttpRequest) -> bool {
    request
        .header_value("Sec-WebSocket-Version")
        .map(|value| value.trim() == "13")
        .unwrap_or(false)
}

/// 101 升级响应头（只算头，不写 socket；测试可直接断言）。
pub fn ws_handshake_response(accept_key: &str) -> HttpResponse {
    let mut response = HttpResponse::new(101);
    response = response.with_header("Upgrade", "websocket");
    response = response.with_header("Connection", "Upgrade");
    response = response.with_header("Sec-WebSocket-Accept", accept_key);
    response
}

/// 写出 101 切换协议（无 Content-Length/无 body）。
fn write_ws_handshake<W: Write>(writer: &mut W, accept_key: &str) -> io::Result<()> {
    let mut head = String::new();
    head.push_str("HTTP/1.1 101 Switching Protocols\r\n");
    head.push_str("Upgrade: websocket\r\n");
    head.push_str("Connection: Upgrade\r\n");
    head.push_str("Sec-WebSocket-Accept: ");
    head.push_str(accept_key);
    head.push_str("\r\n\r\n");
    writer.write_all(head.as_bytes())?;
    writer.flush()
}

/// 升级请求中转发给 REST 鉴权的头（Cookie/token/CSRF 别名；E-4）。
pub fn ws_forward_headers(request: &HttpRequest) -> Vec<(String, String)> {
    const FORWARD_HEADERS: [&str; 6] = [
        "Cookie",
        "X-KHAEN-Token",
        "X-Auth-Token",
        "Authorization",
        "X-KHAEN-CSRF",
        "X-CSRF-Token",
    ];
    let mut headers = Vec::new();
    for name in FORWARD_HEADERS {
        if let Some(value) = request.header_value(name) {
            if !value.trim().is_empty() {
                headers.push((name.to_string(), value.to_string()));
            }
        }
    }
    headers
}

// ---------------------------------------------------------------------------
// Web 服务器（TCP accept + HTTP/1.1 keep-alive + 连接上限）
// ---------------------------------------------------------------------------

/// 每连接 WS 控制面工厂（T26 注入真实 host/engine 实现；默认 REST 桥）。
pub type PlaneFactory =
    Arc<dyn Fn(Vec<(String, String)>) -> Box<dyn ws::WsControlPlane + Send> + Send + Sync>;

/// 真实 Web 入口（T70）：监听 TCP，服务静态页面 + REST + WS。
pub struct WebServer {
    listener: TcpListener,
    rest: Arc<RestService>,
    config: WebConfig,
    connections: Arc<AtomicUsize>,
    plane_factory: Option<PlaneFactory>,
}

impl WebServer {
    /// 默认配置绑定（0.0.0.0:80；T26/T74 实际部署入口）。
    pub fn bind(rest: RestService) -> io::Result<Self> {
        WebServer::bind_with(WebConfig::default(), rest)
    }

    /// 指定配置绑定（监听 socket 保持阻塞：accept 循环靠 accept(2) 休眠等连接，
    /// 空闲时不轮询；见 `run`/`run_until` 与 E-6）。
    pub fn bind_with(config: WebConfig, rest: RestService) -> io::Result<Self> {
        let listener = TcpListener::bind(config.bind_addr.as_str())?;
        Ok(WebServer {
            listener,
            rest: Arc::new(rest),
            config,
            connections: Arc::new(AtomicUsize::new(0)),
            plane_factory: None,
        })
    }

    /// 注入 WS 控制面工厂（T26 真实 host 状态；不注入则用 T42 REST 桥）。
    pub fn with_plane_factory(mut self, factory: PlaneFactory) -> Self {
        self.plane_factory = Some(factory);
        self
    }

    /// 实际监听地址（端口 0 时用于测试）。
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// 配置快照。
    pub fn config(&self) -> &WebConfig {
        &self.config
    }

    /// REST 核心（T26/T74 可读 actual 状态；不做第二个状态源）。
    pub fn rest(&self) -> &Arc<RestService> {
        &self.rest
    }

    /// 纯路由（不碰 socket；测试/T26 内嵌使用）。
    pub fn handle(&self, request: &HttpRequest) -> WebDispatch {
        if request.path == WS_PATH {
            if !request.is_websocket_upgrade() {
                return WebDispatch::Response(HttpResponse::error(
                    400,
                    CODE_INVALID_REQUEST,
                    "expected a WebSocket upgrade request on /ws",
                ));
            }
            return match self.upgrade_websocket(request) {
                Ok((accept_key, auth_headers)) => WebDispatch::WebSocket {
                    accept_key,
                    auth_headers,
                },
                Err(response) => WebDispatch::Response(response),
            };
        }
        if is_rest_path(&request.path) {
            let rest_request = request.to_rest_request();
            let rest_response = self.rest.handle(&rest_request);
            return WebDispatch::Response(HttpResponse::from_rest(rest_response));
        }
        if !static_method_allowed(request.method) {
            return WebDispatch::Response(
                HttpError::new(
                    405,
                    CODE_METHOD_NOT_ALLOWED,
                    "static resources are GET-only on the KHAEN web entry",
                )
                .into_response(),
            );
        }
        WebDispatch::Response(static_asset(&request.path))
    }

    /// 便捷：只取 HTTP 响应（WS 升级请求返回 400；测试/简单调用用）。
    pub fn handle_http(&self, request: &HttpRequest) -> HttpResponse {
        match self.handle(request) {
            WebDispatch::Response(response) => response,
            WebDispatch::WebSocket { .. } => HttpResponse::error(
                400,
                CODE_INVALID_REQUEST,
                "use the WebSocket transport for /ws",
            ),
        }
    }

    /// WS 升级校验 + 鉴权（E-4）。返回 (accept key, 桥接 REST 的鉴权头)。
    fn upgrade_websocket(
        &self,
        request: &HttpRequest,
    ) -> Result<(String, Vec<(String, String)>), HttpResponse> {
        if !is_valid_ws_version(request) {
            return Err(HttpResponse::error(
                426,
                CODE_UNSUPPORTED_VALUE,
                "WebSocket version 13 required",
            )
            .with_header("Sec-WebSocket-Version", "13"));
        }
        let key = match request.header_value("Sec-WebSocket-Key") {
            Some(value) if is_valid_ws_key(value.trim()) => value.trim().to_string(),
            _ => {
                return Err(HttpResponse::error(
                    400,
                    CODE_INVALID_REQUEST,
                    "missing or invalid Sec-WebSocket-Key",
                ))
            }
        };
        let auth_headers = match self.config.ws_auth {
            WsAuthMode::AllowAnonymous => ws_forward_headers(request),
            WsAuthMode::RequireAuth => self.authenticate_ws_request(request)?,
        };
        Ok((ws_accept_key(&key), auth_headers))
    }

    /// WS 鉴权：复用 REST 同一套 token/Cookie 会话（GET /api/auth/session），
    /// Session 会话补 CSRF 头供桥接写命令使用；token 头免 CSRF。
    pub fn authenticate_ws_request(
        &self,
        request: &HttpRequest,
    ) -> Result<Vec<(String, String)>, HttpResponse> {
        let mut headers = ws_forward_headers(request);
        // 非浏览器客户端可用 ?token= 传直接 token（E-4；页面不使用，避免 URL 泄露）。
        if let Some((_, token)) = request
            .query
            .iter()
            .find(|(key, _)| key == "token")
            .filter(|(_, value)| !value.is_empty())
        {
            headers.push(("X-KHAEN-Token".to_string(), token.clone()));
        }
        let mut session_request = RestRequest::get("/api/auth/session");
        session_request.headers = headers.clone();
        let session_response = self.rest.handle(&session_request);
        let mut authenticated = false;
        let mut csrf: Option<String> = None;
        if session_response.status == 200 {
            if let Ok(value) = JsonValue::parse(&session_response.body) {
                authenticated = matches!(value.get("authenticated"), Some(JsonValue::Bool(true)));
                if let Some(JsonValue::Str(token)) = value.get("csrf_token") {
                    csrf = Some(token.clone());
                }
            }
        }
        if !authenticated {
            return Err(HttpResponse::from_rest(session_response));
        }
        headers.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("X-KHAEN-CSRF") && !name.eq_ignore_ascii_case("X-CSRF-Token")
        });
        if let Some(csrf) = csrf {
            headers.push(("X-KHAEN-CSRF".to_string(), csrf));
        }
        Ok(headers)
    }

    /// 创建一条连接的 WS 控制面（默认 T42 REST 桥；T26 可用工厂替换）。
    pub fn new_ws_plane(
        &self,
        auth_headers: Vec<(String, String)>,
    ) -> Box<dyn ws::WsControlPlane + Send> {
        match &self.plane_factory {
            Some(factory) => factory(auth_headers),
            None => Box::new(RestWsPlane::new(Arc::clone(&self.rest), auth_headers)),
        }
    }

    /// 阻塞 accept 循环（生产入口；错误可重试，不静默退出）。
    ///
    /// task-245：监听 socket 用**阻塞 accept**（事件驱动）——空闲时线程睡在 accept(2)，
    /// 无请求时 0 轮询、0 唤醒、CPU≈0；有连接时内核立即唤醒。旧实现是非阻塞 + 紧循环
    /// 轮询，空闲时白烧约 1 个核（板端实测 93% 单核，见 evidence/daemon-cpu-fix/）。
    pub fn run(self: &Arc<Self>) -> io::Result<()> {
        self.listener.set_nonblocking(false)?;
        loop {
            match self.accept_blocking() {
                Ok(Some(stream)) => self.spawn_connection(stream),
                Ok(None) => thread::sleep(Duration::from_millis(1)),
                Err(error) if is_fatal_accept_error(&error) => return Err(error),
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// 可停止 accept 循环（测试/T26 生命周期管理）。
    ///
    /// task-245：事件驱动（空闲阻塞在 accept(2)，无请求时 CPU≈0）。**停止契约**：置位
    /// `stop` 后必须调用 [`WebServer::wake`] 打断阻塞 accept（main.rs 的 SIGTERM/
    /// --exit-after-ms 与 T70 `TestServer` 均已按此接线）。
    pub fn run_until(self: &Arc<Self>, stop: &AtomicBool) -> io::Result<()> {
        self.listener.set_nonblocking(false)?;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.accept_blocking() {
                Ok(Some(stream)) => {
                    if stop.load(Ordering::SeqCst) {
                        // wake() 产生的自连接只用于打断 accept，不当作业务请求服务。
                        drop(stream);
                        return Ok(());
                    }
                    self.spawn_connection(stream);
                }
                Ok(None) => thread::sleep(Duration::from_millis(1)),
                Err(error) if is_fatal_accept_error(&error) => return Err(error),
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// 打断阻塞的 accept（停止路径用；best-effort、幂等）。
    ///
    /// 事件驱动的代价：accept 会一直阻塞，因此停止时需要一个外部唤醒。做法是向自身监听
    /// 地址发一条短连接——内核在 accept 等待中完成握手并唤醒它。调用方在
    /// `stop.store(true, Ordering::SeqCst)` **之后**调用本方法即可。
    pub fn wake(&self) {
        let Ok(addr) = self.listener.local_addr() else {
            return;
        };
        // 绑定 0.0.0.0/:: 时不能直接连 unspecified 地址，改连本机回环。
        let target = match addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port())
            }
            _ => addr,
        };
        let _ = TcpStream::connect_timeout(&target, Duration::from_millis(250));
    }

    /// 一次 accept：阻塞等待连接；中断/would-block 归一为 Ok(None)（监听 socket 为阻塞模式，
    /// 正常情况下不会出现 would-block，仅作防御）。
    fn accept_blocking(&self) -> io::Result<Option<TcpStream>> {
        match self.listener.accept() {
            Ok((stream, _peer)) => Ok(Some(stream)),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// 为一条已接受的连接起服务线程（连接有界；线程创建失败则丢弃该连接，不 panic）。
    fn spawn_connection(self: &Arc<Self>, stream: TcpStream) {
        let server = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("khaen-web-conn".to_string())
            .spawn(move || server.serve_connection(stream));
    }

    /// 服务一条已接受的连接（HTTP keep-alive 循环 + 可选 WS 升级）。
    pub fn serve_connection(&self, stream: TcpStream) {
        // 监听 socket 为阻塞模式（accept 事件驱动），accept 出的连接应已是阻塞；仍显式还原
        // 一次以消除平台差异（否则 read_until 可能立即 WouldBlock -> 400）。
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        let guard = self.track_connection();
        if !self.apply_timeouts(&stream) {
            return;
        }
        let writer_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(_) => return,
        };
        let mut reader = BufReader::new(stream);
        let mut writer = writer_stream;
        if guard.over_limit() {
            // 有界读掉一条请求（Content-Length/head 均有限额），否则关闭时未读 socket 数据
            // 在 Windows 上会触发 RST，客户端可能读不到 503 body。
            let _ = read_http_request(&mut reader);
            let response = HttpResponse::error(
                503,
                CODE_SERVICE_UNAVAILABLE,
                "connection limit reached; retry later",
            );
            let _ = write_http_response(&mut writer, &response, false);
            return;
        }
        let mut served: u64 = 0;
        while served < MAX_REQUESTS_PER_CONNECTION {
            let request = match read_http_request(&mut reader) {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(error) => {
                    let response = error.into_response();
                    let _ = write_http_response(&mut writer, &response, false);
                    break;
                }
            };
            served = served.saturating_add(1);
            let keep_alive = request.keep_alive;
            match self.handle(&request) {
                WebDispatch::Response(response) => {
                    if write_http_response(&mut writer, &response, keep_alive).is_err() {
                        break;
                    }
                    if !keep_alive {
                        break;
                    }
                }
                WebDispatch::WebSocket {
                    accept_key,
                    auth_headers,
                } => {
                    if write_ws_handshake(&mut writer, &accept_key).is_err() {
                        break;
                    }
                    let mut plane = self.new_ws_plane(auth_headers.clone());
                    let context = WebWsContext::new(Arc::clone(&self.rest), auth_headers);
                    let _ = run_ws_connection(&mut reader, &mut writer, plane.as_mut(), &context);
                    break;
                }
            }
        }
        // 尽力优雅关闭写端（读到 EOF/Close 后）。
        let _ = writer.flush();
    }

    /// 连接上限计数（超出 -> 503 并拒绝；guard Drop 归还名额）。
    fn track_connection(&self) -> ConnectionGuard {
        let previous = self.connections.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard {
            counter: Arc::clone(&self.connections),
            over_limit: previous >= self.config.max_connections,
        }
    }

    /// 设置读写超时（避免半开连接永久占线程）；失败视为不可服务。
    fn apply_timeouts(&self, stream: &TcpStream) -> bool {
        let timeout = Duration::from_secs(self.config.io_timeout_secs.max(1));
        stream.set_read_timeout(Some(timeout)).is_ok()
            && stream.set_write_timeout(Some(timeout)).is_ok()
            && stream.set_nodelay(true).is_ok()
    }
}

/// 连接计数 guard（Drop 归还；超限连接在 handler 起始处直接 503）。
pub struct ConnectionGuard {
    counter: Arc<AtomicUsize>,
    over_limit: bool,
}

impl ConnectionGuard {
    /// 是否超限（handler 若超限应立即 503 并结束）。
    pub fn over_limit(&self) -> bool {
        self.over_limit
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// accept 错误是否致命（资源耗尽/非法 fd 等返回；其余重试）。
fn is_fatal_accept_error(error: &io::Error) -> bool {
    !matches!(
        error.kind(),
        io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

// ---------------------------------------------------------------------------
// WebSocket 传输（RFC 6455 子集：文本 JSON 帧 + Ping/Pong/Close；E-1 纯 std）
// ---------------------------------------------------------------------------

/// WS 帧操作码：continuation。
pub const WS_OPCODE_CONTINUATION: u8 = 0x0;
/// WS 帧操作码：text。
pub const WS_OPCODE_TEXT: u8 = 0x1;
/// WS 帧操作码：binary。
pub const WS_OPCODE_BINARY: u8 = 0x2;
/// WS 帧操作码：close。
pub const WS_OPCODE_CLOSE: u8 = 0x8;
/// WS 帧操作码：ping。
pub const WS_OPCODE_PING: u8 = 0x9;
/// WS 帧操作码：pong。
pub const WS_OPCODE_PONG: u8 = 0xA;

/// 服务端关闭码：going away（读超时/服务停止）。
pub const WS_CLOSE_GOING_AWAY: u16 = 1001;

/// 已解出的 ws 消息（拥有数据；交给 T24 WsSession 前再借出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// 文本消息（UTF-8 由 WsSession::handle_bytes 校验；非法 -> 1007）。
    Text(Vec<u8>),
    /// 二进制消息（T24 W01：显式报错，不静默当文本）。
    Binary(Vec<u8>),
    /// Ping（回 Pong 原 payload）。
    Ping(Vec<u8>),
    /// Pong（忽略）。
    Pong(Vec<u8>),
    /// Close（code/reason 仅日志用；T24 审计口径）。
    Close(Option<u16>, String),
}

/// WS 读错误（协议错误带关闭码；IO 错误原样返回）。
#[derive(Debug)]
pub enum WsReadError {
    /// 底层 IO 错误（超时/对端关闭等）。
    Io(io::Error),
    /// 协议错误（发送带 code 的 Close 帧后结束连接）。
    Protocol {
        /// RFC 6455 关闭码。
        code: u16,
        /// 稳定原因文本。
        reason: String,
    },
}

impl From<io::Error> for WsReadError {
    fn from(error: io::Error) -> Self {
        WsReadError::Io(error)
    }
}

/// WS 帧头解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WsFrameHeader {
    fin: bool,
    opcode: u8,
    mask: [u8; 4],
    payload_len: usize,
    header_len: usize,
}

/// 客户端→服务端帧读取器（要求 client mask；支持分片与非分片控制帧交织）。
pub struct WsFrameReader<R> {
    inner: R,
    pending: Vec<u8>,
    eof: bool,
    max_frame_bytes: usize,
    fragment_opcode: Option<u8>,
    fragment_payload: Vec<u8>,
}

impl<R: Read> WsFrameReader<R> {
    /// 新建（max_frame_bytes 单帧/单消息上限）。
    pub fn new(inner: R, max_frame_bytes: usize) -> Self {
        WsFrameReader {
            inner,
            pending: Vec::new(),
            eof: false,
            max_frame_bytes,
            fragment_opcode: None,
            fragment_payload: Vec::new(),
        }
    }

    /// 读入更多字节；返回新增字节数（0 = EOF）。
    fn fill(&mut self) -> Result<usize, WsReadError> {
        if self.eof {
            return Ok(0);
        }
        let mut buffer = [0u8; 4096];
        match self.inner.read(&mut buffer) {
            Ok(0) => {
                self.eof = true;
                Ok(0)
            }
            Ok(read) => {
                self.pending.extend_from_slice(&buffer[..read]);
                Ok(read)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(1),
            Err(error) => Err(WsReadError::Io(error)),
        }
    }

    /// 下一个消息；None = 连接正常 EOF。
    pub fn next_message(&mut self) -> Result<Option<WsMessage>, WsReadError> {
        loop {
            if let Some(header) = self.try_parse_header()? {
                if self.pending.len() < header.header_len + header.payload_len {
                    if self.fill()? == 0 {
                        return Err(protocol_error(
                            1002,
                            "connection closed inside a WebSocket frame",
                        ));
                    }
                    continue;
                }
                let frame = self.take_frame(header);
                if let Some(message) = self.accept_frame(header, frame)? {
                    return Ok(Some(message));
                }
                continue;
            }
            if self.eof {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                return Err(protocol_error(1002, "truncated WebSocket frame"));
            }
            if self.fill()? == 0 && self.pending.is_empty() {
                return Ok(None);
            }
        }
    }

    /// 解析帧头（数据不足返回 None；协议错误返回 Err）。
    fn try_parse_header(&self) -> Result<Option<WsFrameHeader>, WsReadError> {
        let data = &self.pending;
        if data.len() < 2 {
            return Ok(None);
        }
        let byte0 = data[0];
        let byte1 = data[1];
        if byte0 & 0x70 != 0 {
            return Err(protocol_error(1002, "RSV bits must be zero"));
        }
        let fin = byte0 & 0x80 != 0;
        let opcode = byte0 & 0x0F;
        let masked = byte1 & 0x80 != 0;
        if !masked {
            return Err(protocol_error(
                1002,
                "client frames must be masked (RFC 6455 5.1)",
            ));
        }
        let length_code = (byte1 & 0x7F) as usize;
        let mut index = 2usize;
        let payload_len = match length_code {
            126 => {
                if data.len() < index + 2 {
                    return Ok(None);
                }
                let value = u16::from_be_bytes([data[index], data[index + 1]]) as usize;
                index += 2;
                value
            }
            127 => {
                if data.len() < index + 8 {
                    return Ok(None);
                }
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&data[index..index + 8]);
                index += 8;
                let value = u64::from_be_bytes(raw);
                if value > usize::MAX as u64 {
                    return Err(protocol_error(1009, "WebSocket frame is too large"));
                }
                value as usize
            }
            other => other,
        };
        if data.len() < index + 4 {
            return Ok(None);
        }
        let mut mask = [0u8; 4];
        mask.copy_from_slice(&data[index..index + 4]);
        index += 4;
        if payload_len > self.max_frame_bytes {
            return Err(protocol_error(
                1009,
                "WebSocket frame exceeds max_frame_bytes",
            ));
        }
        Ok(Some(WsFrameHeader {
            fin,
            opcode,
            mask,
            payload_len,
            header_len: index,
        }))
    }

    /// 取出完整帧载荷（解 mask）并从 pending 消费。
    fn take_frame(&mut self, header: WsFrameHeader) -> Vec<u8> {
        let start = header.header_len;
        let end = start + header.payload_len;
        let mut payload = self.pending[start..end].to_vec();
        self.pending.drain(..end);
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= header.mask[index % 4];
        }
        payload
    }

    /// 按操作码/分片规则生成消息（控制帧返回 Some；数据帧可能返回 None 表示还需续帧）。
    fn accept_frame(
        &mut self,
        header: WsFrameHeader,
        payload: Vec<u8>,
    ) -> Result<Option<WsMessage>, WsReadError> {
        match header.opcode {
            WS_OPCODE_TEXT | WS_OPCODE_BINARY => {
                if self.fragment_opcode.is_some() {
                    return Err(protocol_error(
                        1002,
                        "new data frame while a fragmented message is pending",
                    ));
                }
                if header.fin {
                    if payload.len() > self.max_frame_bytes {
                        return Err(protocol_error(1009, "WebSocket message too large"));
                    }
                    return Ok(Some(if header.opcode == WS_OPCODE_TEXT {
                        WsMessage::Text(payload)
                    } else {
                        WsMessage::Binary(payload)
                    }));
                }
                self.fragment_opcode = Some(header.opcode);
                self.fragment_payload = payload;
                Ok(None)
            }
            WS_OPCODE_CONTINUATION => {
                let opcode = match self.fragment_opcode {
                    Some(opcode) => opcode,
                    None => {
                        return Err(protocol_error(
                            1002,
                            "continuation frame without a pending message",
                        ))
                    }
                };
                if self.fragment_payload.len() + payload.len() > self.max_frame_bytes {
                    return Err(protocol_error(1009, "WebSocket message too large"));
                }
                self.fragment_payload.extend_from_slice(&payload);
                if header.fin {
                    let payload = std::mem::take(&mut self.fragment_payload);
                    self.fragment_opcode = None;
                    return Ok(Some(if opcode == WS_OPCODE_TEXT {
                        WsMessage::Text(payload)
                    } else {
                        WsMessage::Binary(payload)
                    }));
                }
                Ok(None)
            }
            WS_OPCODE_PING => {
                if !header.fin || payload.len() > 125 {
                    return Err(protocol_error(1002, "invalid ping control frame"));
                }
                Ok(Some(WsMessage::Ping(payload)))
            }
            WS_OPCODE_PONG => {
                if !header.fin || payload.len() > 125 {
                    return Err(protocol_error(1002, "invalid pong control frame"));
                }
                Ok(Some(WsMessage::Pong(payload)))
            }
            WS_OPCODE_CLOSE => {
                if !header.fin || payload.len() > 125 {
                    return Err(protocol_error(1002, "invalid close control frame"));
                }
                let (code, reason) = parse_close_payload(&payload)?;
                Ok(Some(WsMessage::Close(code, reason)))
            }
            other => Err(protocol_error(
                1002,
                &format!("unsupported WebSocket opcode {other}"),
            )),
        }
    }
}

/// 协议错误构造（原因文本有界）。
fn protocol_error(code: u16, reason: &str) -> WsReadError {
    WsReadError::Protocol {
        code,
        reason: reason.to_string(),
    }
}

/// Close 载荷解析（空载荷 = 无 code；非法 UTF-8 原因 -> 1007）。
fn parse_close_payload(payload: &[u8]) -> Result<(Option<u16>, String), WsReadError> {
    if payload.is_empty() {
        return Ok((None, String::new()));
    }
    if payload.len() == 1 {
        return Err(protocol_error(
            1002,
            "close frame payload length 1 is invalid",
        ));
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = match std::str::from_utf8(&payload[2..]) {
        Ok(reason) => reason.to_string(),
        Err(_) => {
            return Err(protocol_error(
                1007,
                "close frame reason is not valid UTF-8",
            ))
        }
    };
    Ok((Some(code), reason))
}

/// 编码服务端文本帧（FIN=1、无 mask）。
pub fn encode_ws_text(text: &str) -> Vec<u8> {
    encode_ws_frame(WS_OPCODE_TEXT, text.as_bytes())
}

/// 编码服务端 Pong 帧。
pub fn encode_ws_pong(payload: &[u8]) -> Vec<u8> {
    encode_ws_frame(WS_OPCODE_PONG, payload)
}

/// 编码服务端 Close 帧（reason 有界并按 UTF-8 边界截断）。
pub fn encode_ws_close(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + WS_CLOSE_REASON_MAX);
    payload.extend_from_slice(&code.to_be_bytes());
    let mut trimmed = reason;
    if trimmed.len() > WS_CLOSE_REASON_MAX - 2 {
        let mut boundary = WS_CLOSE_REASON_MAX - 2;
        while boundary > 0 && !trimmed.is_char_boundary(boundary) {
            boundary -= 1;
        }
        trimmed = &trimmed[..boundary];
    }
    payload.extend_from_slice(trimmed.as_bytes());
    encode_ws_frame(WS_OPCODE_CLOSE, &payload)
}

/// 编码服务端帧（FIN=1、无 mask、长度 7/16/64 位）。
pub fn encode_ws_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | (opcode & 0x0F));
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    frame
}

/// 编码客户端帧（仅测试/调试用：带 mask）。
pub fn encode_ws_client_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x80 | (opcode & 0x0F));
    if payload.len() < 126 {
        frame.push(0x80 | (payload.len() as u8));
    } else if payload.len() <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    for (index, byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[index % 4]);
    }
    frame
}

/// 跑一条 WS 连接（纯函数：reader/writer 可注入；测试用 Cursor 或 socket）。
pub fn run_ws_connection<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    plane: &mut dyn ws::WsControlPlane,
    context: &WebWsContext,
) -> io::Result<()> {
    let mut session = ws::WsSession::new(ws::WsLimits::DEFAULT);
    let mut frames = WsFrameReader::new(reader, WS_MAX_FRAME_BYTES);
    loop {
        let message = match frames.next_message() {
            Ok(Some(message)) => message,
            Ok(None) => {
                let _ = session.abort_pending(plane);
                return Ok(());
            }
            Err(WsReadError::Protocol { code, reason }) => {
                let _ = session.abort_pending(plane);
                let _ = writer.write_all(&encode_ws_close(code, &reason));
                let _ = writer.flush();
                return Ok(());
            }
            Err(WsReadError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let _ = session.abort_pending(plane);
                let _ = writer.write_all(&encode_ws_close(
                    WS_CLOSE_GOING_AWAY,
                    "connection read timeout",
                ));
                let _ = writer.flush();
                return Ok(());
            }
            Err(WsReadError::Io(error)) => return Err(error),
        };
        let reply = match &message {
            WsMessage::Text(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => context.handle_text(&mut session, plane, text),
                Err(_) => session.handle_bytes(plane, bytes),
            },
            WsMessage::Binary(bytes) => {
                session.handle_frame(plane, ws::WsFrame::Binary(bytes.as_slice()))
            }
            WsMessage::Ping(payload) => {
                session.handle_frame(plane, ws::WsFrame::Ping(payload.as_slice()))
            }
            WsMessage::Pong(payload) => {
                session.handle_frame(plane, ws::WsFrame::Pong(payload.as_slice()))
            }
            WsMessage::Close(_, _) => session.handle_frame(plane, ws::WsFrame::Close),
        };
        match reply {
            ws::WsReply::None => {}
            ws::WsReply::Text(text) => {
                writer.write_all(&encode_ws_text(&text))?;
            }
            ws::WsReply::Pong(payload) => {
                writer.write_all(&encode_ws_pong(&payload))?;
            }
            ws::WsReply::Close { code, reason } => {
                writer.write_all(&encode_ws_close(code, &reason))?;
                writer.flush()?;
                return Ok(());
            }
        }
        writer.flush()?;
    }
}

// ---------------------------------------------------------------------------
// SHA-1 / base64（WS 握手；纯 std，RFC 3174 / RFC 4648）
// ---------------------------------------------------------------------------

/// SHA-1 摘要（仅 WS 握手用；不用于安全哈希/凭据）。
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut block = [0u8; 64];
    let full_blocks = data.len() / 64;
    for index in 0..full_blocks {
        block.copy_from_slice(&data[index * 64..index * 64 + 64]);
        sha1_compress(&mut state, &block);
    }
    let remainder = &data[full_blocks * 64..];
    let mut tail = [0u8; 128];
    tail[..remainder.len()].copy_from_slice(remainder);
    tail[remainder.len()] = 0x80;
    let tail_len = if remainder.len() + 1 + 8 <= 64 {
        64
    } else {
        128
    };
    tail[tail_len - 8..tail_len].copy_from_slice(&bit_len.to_be_bytes());
    let mut first_block = [0u8; 64];
    first_block.copy_from_slice(&tail[..64]);
    sha1_compress(&mut state, &first_block);
    if tail_len == 128 {
        let mut second = [0u8; 64];
        second.copy_from_slice(&tail[64..128]);
        sha1_compress(&mut state, &second);
    }
    let mut digest = [0u8; 20];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

/// SHA-1 单块压缩。
fn sha1_compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut words = [0u32; 80];
    for index in 0..16 {
        words[index] = u32::from_be_bytes([
            block[index * 4],
            block[index * 4 + 1],
            block[index * 4 + 2],
            block[index * 4 + 3],
        ]);
    }
    for index in 16..80 {
        let value = words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16];
        words[index] = value.rotate_left(1);
    }
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    for (index, word) in words.iter().enumerate() {
        let (function, constant) = match index {
            0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
            20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
            _ => (b ^ c ^ d, 0xCA62C1D6u32),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(function)
            .wrapping_add(e)
            .wrapping_add(constant)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

/// base64 字母表（RFC 4648 标准）。
const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64 编码（无换行；RFC 4648）。
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        out.push(BASE64_TABLE[(first >> 2) as usize] as char);
        out.push(BASE64_TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_TABLE[(((second & 0x0F) << 2) | (third >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_TABLE[(third & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// base64 解码（严格：拒绝非法字符/长度；忽略尾部空白）。
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut padding = 0usize;
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let mut values = [0u8; 4];
        for (position, byte) in chunk.iter().enumerate() {
            let is_last_chunk = index == bytes.len() / 4 - 1;
            if *byte == b'=' {
                if !is_last_chunk || position < 2 {
                    return None;
                }
                padding += 1;
                values[position] = 0;
                continue;
            }
            if padding > 0 {
                return None;
            }
            values[position] = base64_value(*byte)?;
        }
        out.push((values[0] << 2) | (values[1] >> 4));
        if padding < 2 {
            out.push((values[1] << 4) | (values[2] >> 2));
        }
        if padding == 0 {
            out.push((values[2] << 6) | values[3]);
        }
    }
    Some(out)
}

/// base64 单字符值。
fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// T24 WsControlPlane 的 T42 REST 桥（E-3）与 T70 校准进度 WS 扩展
// ---------------------------------------------------------------------------

/// REST JSON 取字符串字段。
fn json_str(value: &JsonValue, key: &str) -> Option<String> {
    match value.get(key) {
        Some(JsonValue::Str(text)) => Some(text.clone()),
        _ => None,
    }
}

/// REST JSON 取有限数字字段。
fn json_f64(value: &JsonValue, key: &str) -> Option<f64> {
    match value.get(key) {
        Some(JsonValue::Num(number)) if number.is_finite() => Some(*number),
        _ => None,
    }
}

/// REST JSON 取 u64 字段（要求整数值）。
fn json_u64(value: &JsonValue, key: &str) -> Option<u64> {
    let number = json_f64(value, key)?;
    if number < 0.0 || number.fract() != 0.0 || number > 9_007_199_254_740_992.0 {
        return None;
    }
    Some(number as u64)
}

/// REST JSON 取 u32 字段。
fn json_u32(value: &JsonValue, key: &str) -> Option<u32> {
    u32::try_from(json_u64(value, key)?).ok()
}

/// REST JSON 取 bool 字段。
fn json_bool(value: &JsonValue, key: &str) -> Option<bool> {
    match value.get(key) {
        Some(JsonValue::Bool(flag)) => Some(*flag),
        _ => None,
    }
}

/// 稳定 REST 错误码 + message -> WS 错误（同一语义面不出现两套口径；T24 rest_equivalent 反向）。
pub fn ws_error_from_rest(status: u16, body: &str) -> ws::WsOpError {
    let parsed = JsonValue::parse(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| json_str(error, "code"))
        .unwrap_or_else(|| "internal_error".to_string());
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| json_str(error, "message"))
        .unwrap_or_else(|| "REST request failed".to_string());
    let ws_code = match status {
        400 | 404 | 410 => ws::WsErrorCode::InvalidArgument,
        401 | 403 | 429 | 503 => ws::WsErrorCode::Unsupported,
        409 => match code.as_str() {
            "switch_busy" => ws::WsErrorCode::SwitchBusy,
            "device_busy" => ws::WsErrorCode::DeviceBusy,
            "not_prepared" => ws::WsErrorCode::NotPrepared,
            "stale" => ws::WsErrorCode::StalePlan,
            "quarantine_exhausted" => ws::WsErrorCode::QuarantineExhausted,
            _ => ws::WsErrorCode::PrepareBusy,
        },
        413 => ws::WsErrorCode::PayloadTooLarge,
        _ => ws::WsErrorCode::Internal,
    };
    ws::WsOpError::new(ws_code, format!("REST {status} {code}: {message}"))
}

/// T24 WsControlPlane 的 T42 REST 桥（T60 JobManager 由 daemon 以 CalibrationPort 注入，
/// task-226；本桥只消费 T42 REST 语义与 engine/synthetic 事实，不直接持有 job 状态机）。
///
/// E-3 保守口径：REST 快照没有 RT 测量量（busy/水位/ppm/RMS/fader），桥返回不可测值而不是
/// 伪造信号；T26 用真实 host/engine 实现 WsControlPlane 后经 with_plane_factory 替换。
pub struct RestWsPlane {
    rest: Arc<RestService>,
    auth_headers: Vec<(String, String)>,
    prepares: BTreeMap<u32, String>,
}

impl RestWsPlane {
    /// 用 REST 核心与连接鉴权头构造。
    pub fn new(rest: Arc<RestService>, auth_headers: Vec<(String, String)>) -> Self {
        RestWsPlane {
            rest,
            auth_headers,
            prepares: BTreeMap::new(),
        }
    }

    /// 构造 REST 请求（带连接鉴权头）。
    fn build_request(&self, method: Method, path: &str, body: Option<&str>) -> RestRequest {
        let mut request = RestRequest::new(method, path);
        request.headers = self.auth_headers.clone();
        if let Some(body) = body {
            request
                .headers
                .push(("Content-Type".to_string(), "application/json".to_string()));
            request.body = body.as_bytes().to_vec();
        }
        request
    }

    /// 调 REST 并返回 (status, body)；非 2xx 返回 WS 错误（已映射）。
    fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String), ws::WsOpError> {
        let request = self.build_request(method, path, body);
        let response = self.rest.handle(&request);
        if (200..300).contains(&response.status) {
            Ok((response.status, response.body))
        } else {
            Err(ws_error_from_rest(response.status, &response.body))
        }
    }

    /// 调 REST 并解析 JSON 响应。
    fn call_json(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
    ) -> Result<JsonValue, ws::WsOpError> {
        let (_, body) = self.call(method, path, body)?;
        JsonValue::parse(&body)
            .map_err(|_| ws::WsOpError::new(ws::WsErrorCode::Internal, "REST response is not JSON"))
    }

    /// 读 actual 状态快照。
    fn status(&self) -> Result<JsonValue, ws::WsOpError> {
        self.call_json(Method::Get, "/api/runtime/status", None)
    }

    /// actual plan 读回（E-3：plan_version 取 actual_version 尾随十进制；
    /// config_sha256 取 /api/runtime/config 的 actual_config.config_hash，均为 REST actual 值）。
    fn read_actual_plan(&self) -> Result<ws::ActualPlan, ws::WsOpError> {
        let status = self.status()?;
        let actual_version = json_str(&status, "actual_version").unwrap_or_default();
        let digits: String = actual_version
            .chars()
            .rev()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let plan_version = digits.parse::<u32>().map_err(|_| {
            ws::WsOpError::new(
                ws::WsErrorCode::Internal,
                "actual_version has no numeric plan version (host contract)",
            )
        })?;
        if plan_version == 0 {
            return Ok(ws::ActualPlan::none());
        }
        let graph_id_hex = json_str(&status, "graph_id").unwrap_or_default();
        let config = self.call_json(Method::Get, "/api/runtime/config", None)?;
        let actual_config = match config.get("actual_config") {
            Some(value) => value.clone(),
            None => {
                return Err(ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    "REST config response has no actual_config",
                ))
            }
        };
        let config_sha256_hex = json_str(&actual_config, "config_hash").unwrap_or_default();
        let plan = ws::ActualPlan {
            plan_version,
            graph_id_hex,
            config_sha256_hex,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// 把 GET /api/runtime/config 文档规范成 T42 prepare 覆盖对象（只保留合法字段）。
    fn prepare_document(config_json: &str) -> Result<String, ws::WsOpError> {
        let value = ws::json_parse(config_json).map_err(|error| {
            ws::WsOpError::new(
                ws::WsErrorCode::InvalidJson,
                format!("config is not valid JSON: {error}"),
            )
        })?;
        let mut object = match &value {
            ws::JsonValue::Object(entries) => match entries.iter().find(|(key, _)| key == "config")
            {
                Some((_, inner @ ws::JsonValue::Object(_))) => inner.clone(),
                _ => value.clone(),
            },
            _ => {
                return Err(ws::WsOpError::new(
                    ws::WsErrorCode::InvalidArgument,
                    "config must be a JSON object",
                ))
            }
        };
        sanitize_config_document(&mut object);
        let empty = match &object {
            ws::JsonValue::Object(entries) => entries.is_empty(),
            _ => false,
        };
        if empty {
            return Err(ws::WsOpError::new(
                ws::WsErrorCode::InvalidArgument,
                "config document has no supported fields (mode/sample_rate/chunksize/target/coeff)",
            ));
        }
        let body = ws::json_encode(&object).map_err(|error| {
            ws::WsOpError::new(
                ws::WsErrorCode::InvalidArgument,
                format!("config cannot be encoded: {error}"),
            )
        })?;
        Ok(body)
    }
}

impl ws::WsControlPlane for RestWsPlane {
    fn run_state(&self) -> ws::WsRunState {
        match self
            .status()
            .ok()
            .and_then(|status| json_str(&status, "runtime_state"))
        {
            Some(state) => match state.as_str() {
                "Running" | "Draining" => ws::WsRunState::Running,
                "Paused" => ws::WsRunState::Paused,
                "Init" | "Admitted" | "Starting" | "Preparing" => ws::WsRunState::Starting,
                "Stopped" | "Off" | "Idle" => ws::WsRunState::Inactive,
                _ => ws::WsRunState::Error,
            },
            None => ws::WsRunState::Inactive,
        }
    }

    fn stop_reason(&self) -> ws::WsStopReason {
        match self
            .status()
            .ok()
            .and_then(|status| json_str(&status, "runtime_state"))
        {
            Some(state) => match state.as_str() {
                "Faulted" | "SafetyMute" => ws::WsStopReason::PlaybackError,
                "Stopped" => ws::WsStopReason::Done,
                _ => ws::WsStopReason::None,
            },
            None => ws::WsStopReason::None,
        }
    }

    fn config_path(&self) -> String {
        match self.status().ok() {
            Some(status) => format!(
                "khaen-plan:{}",
                json_str(&status, "actual_version").unwrap_or_else(|| "unknown".to_string())
            ),
            None => "khaen-plan:unavailable".to_string(),
        }
    }

    fn config_title(&self) -> String {
        match self.status().ok() {
            Some(status) => format!(
                "KHAEN RT {} {}",
                json_str(&status, "mode").unwrap_or_else(|| "unknown".to_string()),
                status
                    .get("target")
                    .and_then(|target| json_str(target, "layout"))
                    .unwrap_or_else(|| "layout".to_string())
            ),
            None => "KHAEN RT plan".to_string(),
        }
    }

    fn volume_db(&self) -> f64 {
        self.status()
            .ok()
            .and_then(|status| json_f64(&status, "volume_db"))
            .unwrap_or(f64::NAN)
    }

    fn set_volume_db(&mut self, db: f64) -> Result<f64, ws::WsOpError> {
        let body = format!("{{\"db\": {db}}}");
        let response = self.call_json(Method::Put, "/api/runtime/volume", Some(&body))?;
        json_f64(&response, "volume_db").ok_or_else(|| {
            ws::WsOpError::new(
                ws::WsErrorCode::Internal,
                "REST volume actual ack is missing volume_db",
            )
        })
    }

    fn mute(&self) -> bool {
        self.status()
            .ok()
            .and_then(|status| json_bool(&status, "mute"))
            .unwrap_or(false)
    }

    fn set_mute(&mut self, mute: bool) -> Result<bool, ws::WsOpError> {
        let body = format!("{{\"mute\": {mute}}}");
        let response = self.call_json(Method::Put, "/api/runtime/mute", Some(&body))?;
        json_bool(&response, "mute").ok_or_else(|| {
            ws::WsOpError::new(
                ws::WsErrorCode::Internal,
                "REST mute actual ack is missing mute",
            )
        })
    }

    fn capture_rate_hz(&self) -> u32 {
        self.status()
            .ok()
            .and_then(|status| json_u32(&status, "sample_rate"))
            .unwrap_or(0)
    }

    fn capture_channel_count(&self) -> u32 {
        match self.status() {
            Ok(status) => status
                .get("target")
                .and_then(|target| json_u32(target, "channels"))
                .unwrap_or(0),
            Err(_) => 0,
        }
    }

    fn clipped_samples(&self) -> u64 {
        self.status()
            .ok()
            .and_then(|status| json_u64(&status, "clipped_samples"))
            .unwrap_or(0)
    }

    fn processing_load(&self) -> f64 {
        // E-3：REST 快照无 RT busy 统计；返回 0 且不伪造（T26 真实 plane 覆盖）。
        0.0
    }

    fn buffer_level_frames(&self) -> i64 {
        // E-3：REST 快照无播放队列水位；0 = 不可测（T26 覆盖）。
        0
    }

    fn rate_adjust_ppm(&self) -> f64 {
        // E-3：REST 快照无唯一 clock servo 调整量；本值不可用（见 rate_adjust_available）。
        f64::NAN
    }

    fn rate_adjust_available(&self) -> bool {
        // F-28：没有唯一 clock servo 读数时必须显式不可用，绝不用 0.0 冒充；
        // T26 用真实 host/engine 实现 WsControlPlane 后由 with_plane_factory 替换。
        false
    }

    fn config_document(&self) -> String {
        match self.call(Method::Get, "/api/runtime/config", None) {
            Ok((_, body)) => body,
            Err(error) => format!(
                "{{\"error\":{{\"code\":\"{}\",\"message\":\"config unavailable\"}}}}",
                error.code().as_str()
            ),
        }
    }

    fn capture_signal_rms_db(&self) -> Vec<f64> {
        // E-3/W18：无独立采集计量时按 T24 约定静音 floor 上报（不伪造信号；T26 覆盖）。
        let channels = self.capture_channel_count() as usize;
        vec![ws::RMS_FLOOR_DB; channels]
    }

    fn fader_volume_db(&self, _fader_id: i32) -> Option<f64> {
        // P1 无内容上混 daemon，因此不分配 fader（W19：未分配返回 None）。
        None
    }

    fn set_fader_volume_db(&mut self, _fader_id: i32, _db: f64) -> Result<f64, ws::WsOpError> {
        Err(ws::WsOpError::new(
            ws::WsErrorCode::UnknownFader,
            "P1 has no content-upmix daemon; no fader is allocated",
        ))
    }

    fn actual_plan(&self) -> ws::ActualPlan {
        self.read_actual_plan()
            .unwrap_or_else(|_| ws::ActualPlan::none())
    }

    fn prepare_config_json(
        &mut self,
        config_json: &str,
    ) -> Result<ws::PreparedConfig, ws::WsOpError> {
        let body = Self::prepare_document(config_json)?;
        let response = self.call_json(Method::Post, "/api/runtime/prepare", Some(&body))?;
        let plan_version = json_u32(&response, "prepared_plan_version").ok_or_else(|| {
            ws::WsOpError::new(
                ws::WsErrorCode::Internal,
                "REST prepare ack is missing prepared_plan_version",
            )
        })?;
        let expected_workers = json_u32(&response, "expected_workers").ok_or_else(|| {
            ws::WsOpError::new(
                ws::WsErrorCode::Internal,
                "REST prepare ack is missing expected_workers",
            )
        })?;
        let prepared = ws::PreparedConfig {
            plan_version,
            graph_id_hex: json_str(&response, "prepared_graph_id").ok_or_else(|| {
                ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    "REST prepare ack is missing prepared_graph_id",
                )
            })?,
            config_sha256_hex: json_str(&response, "prepared_config_sha256").ok_or_else(|| {
                ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    "REST prepare ack is missing prepared_config_sha256",
                )
            })?,
            expected_workers: u8::try_from(expected_workers).map_err(|_| {
                ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    "REST prepare ack expected_workers out of range",
                )
            })?,
            admitted_bytes: json_u64(&response, "admitted_bytes").unwrap_or(0),
            admission_note: json_str(&response, "admission").unwrap_or_default(),
        };
        prepared.validate()?;
        if let Some(JsonValue::Str(prepare_id)) = response.get("prepare_id") {
            self.prepares.insert(plan_version, prepare_id.clone());
        }
        Ok(prepared)
    }

    fn commit_prepared(
        &mut self,
        prepared: &ws::PreparedConfig,
    ) -> Result<ws::CommitAck, ws::WsOpError> {
        let prepare_id = self
            .prepares
            .get(&prepared.plan_version)
            .cloned()
            .ok_or_else(|| {
                ws::WsOpError::new(
                    ws::WsErrorCode::NotPrepared,
                    "no REST prepare is registered for this plan_version",
                )
            })?;
        let body = format!(
            "{{\"prepare_id\": \"{}\"}}",
            escape_json_string(&prepare_id)
        );
        let response = self.call_json(Method::Post, "/api/runtime/commit", Some(&body))?;
        let required = |key: &str| -> Result<u64, ws::WsOpError> {
            json_u64(&response, key).ok_or_else(|| {
                ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    format!("REST commit ack is missing {key}"),
                )
            })
        };
        let ack = ws::CommitAck {
            plan_version: u32::try_from(required("plan_version")?).map_err(|_| {
                ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    "commit plan_version out of range",
                )
            })?,
            previous_plan_version: u32::try_from(required("previous_plan_version")?).map_err(
                |_| {
                    ws::WsOpError::new(
                        ws::WsErrorCode::Internal,
                        "commit previous_plan_version out of range",
                    )
                },
            )?,
            graph_id_hex: json_str(&response, "graph_id").unwrap_or_default(),
            config_sha256_hex: json_str(&response, "config_sha256").unwrap_or_default(),
            expected_workers: u8::try_from(required("expected_workers")?).map_err(|_| {
                ws::WsOpError::new(
                    ws::WsErrorCode::Internal,
                    "commit expected_workers out of range",
                )
            })?,
            blocks_committed: required("blocks_committed")?,
            ack_mask: u32::try_from(required("ack_mask")?).map_err(|_| {
                ws::WsOpError::new(ws::WsErrorCode::Internal, "commit ack_mask out of range")
            })?,
            switched_at_block: json_u64(&response, "switched_at_block").unwrap_or(0),
            rolled_back: json_bool(&response, "rolled_back").unwrap_or(false),
        };
        self.prepares.remove(&prepared.plan_version);
        Ok(ack)
    }

    fn abort_prepared(&mut self, prepared: &ws::PreparedConfig) -> Result<(), ws::WsOpError> {
        let prepare_id = match self.prepares.remove(&prepared.plan_version) {
            Some(prepare_id) => prepare_id,
            None => return Ok(()),
        };
        let body = format!(
            "{{\"prepare_id\": \"{}\"}}",
            escape_json_string(&prepare_id)
        );
        self.call(Method::Post, "/api/runtime/abort", Some(&body))?;
        Ok(())
    }
}

/// 规范化 T42 prepare 文档：只保留 RuntimeConfig 合法字段，并剥掉 T42 JSON 投影里的派生键
/// （target.target_hash / coeff 等展示字段），未知字段由 REST 严格白名单继续拒收。
fn sanitize_config_document(value: &mut ws::JsonValue) {
    const CONFIG_FIELDS: [&str; 5] = ["mode", "sample_rate", "chunksize", "target", "coeff"];
    let ws::JsonValue::Object(entries) = value else {
        return;
    };
    entries.retain(|(key, _)| CONFIG_FIELDS.contains(&key.as_str()));
    if let Some((_, target)) = entries.iter_mut().find(|(key, _)| key == "target") {
        if let ws::JsonValue::Object(fields) = target {
            const TARGET_FIELDS: [&str; 5] =
                ["layout", "channels", "profile", "roles", "low_band_dacs"];
            fields.retain(|(key, _)| TARGET_FIELDS.contains(&key.as_str()));
            if let Some((_, roles)) = fields.iter_mut().find(|(key, _)| key == "roles") {
                if let ws::JsonValue::Array(items) = roles {
                    for item in items {
                        if let ws::JsonValue::Object(entry) = item {
                            entry.retain(|(key, _)| matches!(key.as_str(), "role" | "dac"));
                        }
                    }
                }
            }
        }
    }
    if let Some((_, coeff)) = entries.iter_mut().find(|(key, _)| key == "coeff") {
        if let ws::JsonValue::Object(fields) = coeff {
            fields.retain(|(key, _)| matches!(key.as_str(), "version" | "sha256"));
        }
    }
}

/// JSON 字符串转义（仅用于桥内部构造 prepare_id 字段；prepare_id 已受 REST 字符集约束）。
fn escape_json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// WS 文本入口：T24 命令交给 WsSession；T70 校准扩展命令直接走 T42 R48
// ---------------------------------------------------------------------------

/// 每条 WS 连接一个上下文（REST 核心 + 连接鉴权头；扩展命令用）。
pub struct WebWsContext {
    rest: Arc<RestService>,
    auth_headers: Vec<(String, String)>,
}

impl WebWsContext {
    /// 构造。
    pub fn new(rest: Arc<RestService>, auth_headers: Vec<(String, String)>) -> Self {
        WebWsContext { rest, auth_headers }
    }

    /// 处理一个文本帧：T70 扩展命令优先，其余完全交给 T24 WsSession（同键回复/错误口径一致）。
    pub fn handle_text(
        &self,
        session: &mut ws::WsSession,
        plane: &mut dyn ws::WsControlPlane,
        text: &str,
    ) -> ws::WsReply {
        if let Ok(value) = ws::json_parse(text) {
            if let Ok(request) = ws::decode_request(&value) {
                if WS_CAL_COMMANDS.contains(&request.command) {
                    return match self.cal_command(request.command, request.payload_or_null()) {
                        Ok(value) => ws::WsReply::Text(encode_ws_ok(request.command, &value)),
                        Err(error) => ws::WsReply::Text(encode_ws_error(request.command, &error)),
                    };
                }
            }
        }
        session.handle_text(plane, text)
    }

    /// 校准进度扩展命令（GetCalibrationJobs / GetCalibrationJob）。
    fn cal_command(
        &self,
        command: &str,
        payload: &ws::JsonValue,
    ) -> Result<ws::JsonValue, ws::WsOpError> {
        let (method, path) = match command {
            "GetCalibrationJobs" => (Method::Get, "/api/calibration/jobs".to_string()),
            "GetCalibrationJob" => {
                let job_id = payload.as_str().ok_or_else(|| {
                    ws::WsOpError::new(
                        ws::WsErrorCode::InvalidArgument,
                        "GetCalibrationJob expects a job id string",
                    )
                })?;
                if job_id.is_empty()
                    || job_id.len() > 64
                    || !job_id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                {
                    return Err(ws::WsOpError::new(
                        ws::WsErrorCode::InvalidArgument,
                        "invalid calibration job id",
                    ));
                }
                (Method::Get, format!("/api/calibration/jobs/{job_id}"))
            }
            _ => {
                return Err(ws::WsOpError::new(
                    ws::WsErrorCode::UnknownCommand,
                    format!("unsupported command: {command}"),
                ))
            }
        };
        let mut request = RestRequest::new(method, path);
        request.headers = self.auth_headers.clone();
        let response = self.rest.handle(&request);
        if !(200..300).contains(&response.status) {
            return Err(ws_error_from_rest(response.status, &response.body));
        }
        ws::json_parse(&response.body).map_err(|_| {
            ws::WsOpError::new(
                ws::WsErrorCode::Internal,
                "calibration REST response is not JSON",
            )
        })
    }
}

/// 同键 Ok 回复（与 T24 encode_ok_reply 同形）。
pub fn encode_ws_ok(command: &str, value: &ws::JsonValue) -> String {
    let reply = ws::json_object(vec![(
        command,
        ws::json_object(vec![
            ("result", ws::JsonValue::String("Ok".to_string())),
            ("value", value.clone()),
        ]),
    )]);
    ws::json_encode(&reply).unwrap_or_else(|_| {
        "{\"error\":{\"code\":\"internal\",\"message\":\"reply encode failed\"}}".to_string()
    })
}

/// 同键错误回复（与 T24 encode_error_reply 同形）。
pub fn encode_ws_error(command: &str, error: &ws::WsOpError) -> String {
    let reply = ws::json_object(vec![(
        command,
        ws::json_object(vec![("result", error.to_json())]),
    )]);
    ws::json_encode(&reply).unwrap_or_else(|_| {
        "{\"error\":{\"code\":\"internal\",\"message\":\"reply encode failed\"}}".to_string()
    })
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T70 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod web_tests {
    use super::*;

    include!("../tests/T70/web_contract.rs");
}
