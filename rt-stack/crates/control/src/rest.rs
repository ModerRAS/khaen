//! [NONRT] 自有 REST 控制面（WBS T42；compat R01-R51 / X01-X22） ——WBS T42 写域。
//!
//! 本模块实现新栈自己的 REST 语义层（不是 CamillaDSP/WS shim，也不依赖旧
//! khaen-audio-web）：配置/模式/状态/volume/mute/target/coeff 版本、鉴权/冲突/统一错误
//! envelope/API 版本/unknown 拒收、以及 prepare->commit 的 actual ack。HTTP 服务器
//! 与真实 host/ALSA/校准 daemon 接线属 T26/T70；本模块只提供 transport-agnostic 的
//! RestRequest -> RestResponse 处理核心，可被任意 HTTP/WS 传输层直接调用。
//!
//! ## 权威输入（只读）
//! - WBS T42 行与 §1/§7（scratch/lead/planner/TSPI-RT-STACK-WBS.md）
//! - DSP-HOST-DECISION §2/§5/§6（新 daemon job/状态/prepare/commit/actual-version）
//! - rt-stack/spec/compat-matrix.md R01-R51、§2.4、X01-X22、K02/K05/K14/K15/K53/K57
//! - rt-stack/spec/core-v1.md §5.3（PlanCommitted 前不得改输出；REST 的 applied 只能在
//!   plan_version 已出现在完整 block 且 4 worker all-ack 之后返回）、§9.2（RtStatus 码）、
//!   §6.1/I11（runtime 仅 64/128 chunksize）、Q-11
//! - rt-stack/README.md §3（写域纪律）与 tests/T42/README.md（include! 自注册）
//!
//! ## 契约行（逐条落点）
//! - R01 healthz 匿名；安全头；version/mock/uptime_secs/now_unix。
//! - R02-R06 鉴权：status/setup/login/session/logout；401/403/409/429 语义；header token
//!   免 CSRF，Cookie session 的写方法必须带 CSRF（K03）。
//! - R07/R10/R11/R13/R14 面的 P1 语义：mode GET/PUT（202 接受/409 在途/403 未启用/
//!   400 未知值，接受 toslink/target_multichannel）、audio/format（runtime 仅 64/128，
//!   其它值显式 400/unsupported，不静默夹取）。
//! - **F-21 落地口径**：R01-R06/R10-R14/R47-R49 已注册路由，R50/R51 为横切语义；
//!   R07-R09/R15-R46 按 compat-matrix §3.7 为 `ADAPT-DEFERRED`/`KEEP-DEFERRED`
//!   （P1 不注册路由，`/api/*` 未知路径按 X01 JSON 404，不得用空壳 200/假语义冒充）；
//!   矩阵↔路由一致性由 tests/T42/matrix_routes.rs 自动核对（含 6 条负向用例）。
//! - R48 校准 job start/status/list/cancel/apply/retest/rollback：本模块只做 REST 语义与
//!   状态投影；真实 job 状态机在 T60。通过 CalibrationPort trait 注入：daemon（main.rs）
//!   默认挂 calibration_job_port.rs 的真实 T60 JobManager 适配（离线合成 backend，响应显式
//!   engine/synthetic），real/off/无 port 时显式 503 service_unavailable，不伪造 job、不假 ack。
//! - R49 新栈 runtime/control REST：GET/PUT /api/runtime/{status,config,volume,mute,
//!   mode,target,coeff}，POST /api/runtime/{prepare,commit,abort}；响应带 actual
//!   config/graph/coeff/target version+hash；prepare 与 commit 分离；冲突 409；幂等
//!   （重复 commit 同一 prepare_id 返回同一 actual；重复 volume/mute 返回 actual）。
//! - R50 错误 envelope {"error":{"code","message"}}；可选请求头 X-TSPI-API-Version
//!   缺省 v1，仅显式不兼容版本 400 unsupported_api_version；响应头回
//!   X-TSPI-API-Version: 1；不引入 422。
//! - R51/X01：/api/* 未知路径 JSON 404 not_found；已知路径错误方法 JSON 405；
//!   非 /api 未知路径同样 JSON 404（静态资源由 T70 web 层在 REST fallback 前处理）。
//! - X02/X03：请求体严格字段白名单；未知字段 400 unknown_field（报字段名）；缺失
//!   400 missing_field；类型/取值错误 400 invalid_field；重复键/非法 JSON 400
//!   invalid_json；NaN/Inf/超深嵌套拒绝；body 超限 413 payload_too_large。
//! - X14 API 版本；X15 退役端点：一般 410 retired + replacement，唯一例外 R47
//!   input-mode（GET 200 not_supported / PUT 400 not_supported）。
//! - X21 错误不泄露 token/密码/内部路径；X22 大小上限显式。
//! - K14/R48 job 状态全集（queued/preflight/.../independent_holdout/complete/failed/
//!   cancelled/rolled_back）；apply 必须带回 applied_version，否则 500 internal_error
//!   （不假 ack）。
//! - K53 volume/mute：-150..+50 dB、mute 独立、actual 回读、幂等。
//!
//! ## Cursor 审计修复口径（2026-11-17，F-13/F-14/F-28/F-30）
//! - F-13：`RuntimeEngine::engine_info()` 必须声明 engine/synthetic；commit/prepare/status
//!   回执原样带出 `engine`/`synthetic`/`actual`。内存 engine 恒 synthetic=true（页面不得
//!   显示成 actual）；真实 host engine（T26）才返回 actual=true。
//! - F-14：layout="4.1.2" 时 `TargetSpec::validate` 逐项比对 P1_412_ROLES/P1_412_CHANNELS
//!   （SUB1 固定槽 7、ch6 空、容器 8）；C/SUB2/SUB3 等白名单角色不属于 4.1.2，
//!   违反返回稳定码 `layout_roles_mismatch`（不再只按 channels 放行）。
//! - F-28：PUT /api/mode 的 ack 必须带 T44 `ModeController` 证据（epoch/唯一 servo/
//!   回滚）；`RuntimeEngine::mode_evidence()` 为 None 时回执显式 synthetic，
//!   `mode_actual` 恒 false（不得把内存 engine 的 switched 当 actual）。
//! - F-30：模式回滚用 control 层扩展码 124（compat::ws::RT_STATUS_MODE_ROLLED_BACK），
//!   REST 映射 409 `rolled_back`；119 只表示 plan stale。
//!
//! ## actual commit ack（不假 ack）
//! POST /api/runtime/prepare 只调用 engine 的 admit/prepare（不改输出），返回
//! prepare_id + planned version；POST /api/runtime/commit 只在 engine 真正 commit
//! 成功后返回 200，并原样带出 engine 的 actual config_version/graph_id/coeff/target
//! hash/epoch；engine 返回非法 ack（版本空、graph_id 非 64 位小写 hex）时返回
//! 500 internal_error 而不是把坏值当成功。真实 engine 由 T26 用 rt-host
//! swap::PlanRegistry（T20：admit->stage->arm->block 边界 commit->4 worker ack）实现，
//! 语义要求见 core-v1 §5.3 与 T20 evidence；本模块只消费该契约，不伪造 commit。
//!
//! ## 契约偏差/接口边界（已在 evidence/T42/MANIFEST.md 登记）
//! - E-1：control crate 无 serde_json 等第三方依赖（README §2 只允许 rt-host 的 Linux
//!   alsa）；本模块自带严格 JSON 子集解析/序列化（拒绝 NaN/Inf/重复键/未知字段）。
//! - E-2：无 argon2 依赖；密码 KDF 走内置 PBKDF2-HMAC-SHA256（RFC 8018），密码从不明文
//!   保存。KEEP R03 要求 Argon2id —— integrator 加 argon2 依赖后可替换 KDF 实现；当前
//!   默认记为未达 Argon2id，不冒充。
//! - E-3（task-226 收口）：校准 job 的真实 T60 实现经 CalibrationPort trait 注入；daemon
//!   需要构造真实 port，故 control 依赖 khaen-calibration（crates/control/Cargo.toml 由
//!   integrator 复核，见 evidence/cursor-fix-rest-calibport/MANIFEST.md §5）。trait 保持可
//!   注入，测试仍用 MemoryCalibration/ScriptedCalibration 替身；未接线时 503（不是空 200）。
//! - E-4：模式切换完整状态机/独占/回滚属 T44；本模块提供 REST 语义与 RuntimeEngine
//!   的 switch_mode 契约，T44 提供真实 engine/mode backend。
//!
//! ## 测试（include! 自注册，integrator 不要再写 public_registration.rs）
//! 私有测试在 crates/control/tests/T42/rest_contract.rs，经文件末
//! #[cfg(test)] mod rest_tests { include!("../tests/T42/rest_contract.rs"); } 进入 lib
//! 单测构建；运行 cargo test -p khaen-control rest。全部离线：不打开设备、不启服务、
//! 不跑真实 host。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use khaen_compat::ws;
use khaen_core::rt::{RtStatus, BLOCK_SIZES, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ};

// ---------------------------------------------------------------------------
// 常量（WBS T42 / compat R01-R51 / core-v1 §3/§6.1）
// ---------------------------------------------------------------------------

/// REST API 版本（X14/R50；缺省 v1，仅显式不兼容版本拒绝）。
pub const API_VERSION: u32 = 1;
/// 请求/响应 API 版本头（X14）。
pub const API_VERSION_HEADER: &str = "X-TSPI-API-Version";
/// 直接 API token 头（R04/token fallback；免 CSRF）。
pub const TOKEN_HEADER: &str = "X-TSPI-Token";
/// header token 的 Bearer 形式（Authorization: Bearer <token>）。
pub const AUTHORIZATION_HEADER: &str = "Authorization";
/// 会话 Cookie 名（R03-R06）。
pub const SESSION_COOKIE: &str = "khaen_session";
/// 写方法 CSRF 头（K03；Cookie session 必填，header token 免）。
pub const CSRF_HEADER: &str = "X-TSPI-CSRF";
/// CSRF 头的兼容别名（旧客户端）。
pub const CSRF_HEADER_ALIAS: &str = "X-CSRF-Token";
/// runtime 状态 schema（X17：状态必须带版本身份）。
pub const RUNTIME_STATUS_SCHEMA: &str = "tspi-runtime-status/v1";
/// runtime 配置 schema。
pub const CONFIG_SCHEMA: &str = "tspi-runtime-config/v1";
/// 模式状态 schema（R10/A04 tspi-audio/v1 的新栈替代 schema）。
pub const MODE_STATUS_SCHEMA: &str = "tspi-mode-status/v1";
/// 音频格式 schema（R13）。
pub const AUDIO_FORMAT_SCHEMA: &str = "tspi-audio-format/v1";
/// 新栈构建版本（R01 version = 新栈构建版本）。
pub const REST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 单请求体上限（X22）。
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// 会话 TTL 默认值（R05/R06）。
pub const DEFAULT_SESSION_TTL_SECS: u64 = 8 * 3600;
/// 并发会话上限。
pub const MAX_SESSIONS: usize = 16;
/// 连续登录失败上限（R04：失败 5 次 429）。
pub const MAX_FAILED_LOGINS: u32 = 5;
/// 默认锁定时长（秒）。
pub const DEFAULT_LOCKOUT_SECS: u64 = 30;
/// 密码最小长度（R03：>=8）。
pub const MIN_PASSWORD_LEN: usize = 8;
/// 推荐密码长度（R02）。
pub const RECOMMENDED_PASSWORD_LEN: usize = 24;
/// 默认 PBKDF2 迭代数（E-2：非 Argon2id；见文件头偏差）。
pub const DEFAULT_PBKDF2_ITERATIONS: u32 = 10_000;
/// volume 下限 dB（R19/P07/R49）。
pub const VOLUME_MIN_DB: f64 = -150.0;
/// volume 上限 dB（R19/P07/R49）。
pub const VOLUME_MAX_DB: f64 = 50.0;
/// 默认采样率（core-v1 §3.2；R13）。
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
/// 默认 chunksize（生产 128；runtime 仅 64/128）。
pub const DEFAULT_CHUNKSIZE: u16 = 128;
/// commit 幂等重放的保留条数（R49 幂等语义；有界，防无界增长）。
pub const MAX_COMMIT_REPLAYS: usize = 64;
/// 失效 prepare 记录保留条数（用于 409 stale 判定）。
pub const MAX_INVALIDATED_PREPARES: usize = 64;
/// 准备成功的 admission 摘要最大字符数（防坏 engine 塞巨串）。
pub const MAX_ADMISSION_TEXT: usize = 128;
/// 版本/哈希类字段最大字符数。
pub const MAX_VERSION_CHARS: usize = 64;
/// 布局/配置字符串最大字符数。
pub const MAX_LAYOUT_CHARS: usize = 32;

/// P1 模式取值（A01/R10/R11；未知值 400）。
pub const ALLOWED_MODES: [&str; 5] = ["off", "toslink", "target_multichannel", "aes67", "dante"];
/// P1 已知输出角色（Y05/Y11；SUB4 是旧 Web 模板错误值，不在白名单）。
pub const ALLOWED_ROLES: [&str; 10] = [
    "FL", "FR", "C", "SL", "SR", "SUB1", "SUB2", "SUB3", "HFL", "HFR",
];
/// P1 4.1.2 布局名（Y11；唯一 role->DAC 表的布局键）。
pub const P1_412_LAYOUT: &str = "4.1.2";
/// P1 4.1.2 唯一 role->DAC（WBS §3 / layout-presets.yml:219-229；0-based，ch6 空）。
pub const P1_412_ROLES: [(&str, u8); 7] = [
    ("FL", 0),
    ("FR", 1),
    ("SL", 2),
    ("SR", 3),
    ("HFL", 4),
    ("HFR", 5),
    ("SUB1", 7),
];
/// P1 4.1.2 容器宽度（8ch 容器 7 个物理角色，ch6 空）。
pub const P1_412_CHANNELS: u16 = 8;
/// P1 低带 dense 参与者->DAC 表（WBS §3：0..4 -> DAC 0/1/2/3/7；由 T57 解析/hash 覆盖）。
pub const P1_412_LOW_BAND_DACS: [u8; 5] = [0, 1, 2, 3, 7];
/// 校准 job 状态全集（DEC §2.6 / T60 JobState::as_str；R48/K14）。
pub const CAL_JOB_STATES: [&str; 15] = [
    "queued",
    "preflight",
    "sweep_capture",
    "quality_check",
    "fit_p",
    "solve_g",
    "compile",
    "prepared",
    "apply",
    "remeasure",
    "independent_holdout",
    "complete",
    "failed",
    "cancelled",
    "rolled_back",
];
/// 校准 job 终态（R48：取消/失败只回 last-good/mute）。
pub const CAL_JOB_TERMINAL_STATES: [&str; 4] = ["complete", "failed", "cancelled", "rolled_back"];
/// 校准 job 类型（T60 JobKind）。
pub const CAL_JOB_KINDS: [&str; 3] = ["full", "retest", "emperor"];
/// 校准扩展 stage（level/delay/bass；host 字段未落地时 501 pending_host_field）。
pub const CAL_EXTRA_STAGES: [&str; 5] = ["level", "delay", "bass", "fir", "modal"];

/// 所有响应都附带的 API 版本头值（R50）。
pub const API_VERSION_RESPONSE_VALUE: &str = "1";
/// R01/R50 要求的安全响应头（CSP/X-Frame-Options/nosniff/Referrer-Policy/no-store）。
pub const SECURITY_HEADERS: [(&str, &str); 5] = [
    (
        "Content-Security-Policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    ("X-Frame-Options", "DENY"),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Cache-Control", "no-store"),
];

// ---------------------------------------------------------------------------
// 错误码与错误 envelope（R50/X01-X03/X15/X21）
// ---------------------------------------------------------------------------

/// 未知路径/资源（X01）。
pub const CODE_NOT_FOUND: &str = "not_found";
/// 已知路径错误方法（X01）。
pub const CODE_METHOD_NOT_ALLOWED: &str = "method_not_allowed";
/// 未认证（R05/R50）。
pub const CODE_UNAUTHORIZED: &str = "unauthorized";
/// 已认证但无权限/CSRF 失败（K03/X21）。
pub const CODE_FORBIDDEN: &str = "forbidden";
/// 非法 JSON（X03）。
pub const CODE_INVALID_JSON: &str = "invalid_json";
/// 未知字段（X02；message 必须带字段名）。
pub const CODE_UNKNOWN_FIELD: &str = "unknown_field";
/// 缺失必填字段（X03）。
pub const CODE_MISSING_FIELD: &str = "missing_field";
/// 字段类型/取值错误（X03）。
pub const CODE_INVALID_FIELD: &str = "invalid_field";
/// 取值不在允许集合（R13/R14 chunksize；X04 mode 除外）。
pub const CODE_UNSUPPORTED_VALUE: &str = "unsupported_value";
/// 未知 mode（X04；message 列出允许值）。
pub const CODE_UNKNOWN_MODE: &str = "unknown_mode";
/// 未知输出角色（X19；SUB4 等）。
pub const CODE_UNKNOWN_ROLE: &str = "unknown_role";
/// 未知 coeff 版本/哈希（X18/Y24）。
pub const CODE_UNKNOWN_COEFF: &str = "unknown_coeff";
/// 不支持的 API 版本（X14）。
pub const CODE_UNSUPPORTED_API_VERSION: &str = "unsupported_api_version";
/// 已退役端点（X15）。
pub const CODE_RETIRED: &str = "retired";
/// R47 例外：显式 P1 不支持（内容上混）。
pub const CODE_NOT_SUPPORTED: &str = "not_supported";
/// 一般冲突（R49/R11）。
pub const CODE_CONFLICT: &str = "conflict";
/// 未 prepare 就 commit（R49；409）。
pub const CODE_NOT_PREPARED: &str = "not_prepared";
/// stale plan/prepare（core-v1 119；X18；409）。
pub const CODE_STALE: &str = "stale";
/// 忙（R11 在途切换）。
pub const CODE_BUSY: &str = "busy";
/// 切换在途（core-v1 120；R11；409）。
pub const CODE_SWITCH_BUSY: &str = "switch_busy";
/// 设备 owner 冲突（core-v1 117；R48 校准独占；409）。
pub const CODE_DEVICE_BUSY: &str = "device_busy";
/// 设备实际参数不满足（core-v1 118；409）。
pub const CODE_DEVICE_PARAMS: &str = "device_params";
/// 登录失败次数超限（R04；429）。
pub const CODE_RATE_LIMITED: &str = "rate_limited";
/// 请求体超限（X22；413）。
pub const CODE_PAYLOAD_TOO_LARGE: &str = "payload_too_large";
/// 后端未接线/不可用（R16/R48；503）。
pub const CODE_SERVICE_UNAVAILABLE: &str = "service_unavailable";
/// host 字段尚未落地（501；level/delay/bass 的 mic/cal_file/lane 结果）。
pub const CODE_PENDING_HOST_FIELD: &str = "pending_host_field";
/// 内部错误（坏 engine ack/不变量；500；不假成功）。
pub const CODE_INTERNAL_ERROR: &str = "internal_error";
/// admission 失败（RtStatus 114-116 映射；400）。
pub const CODE_ADMISSION_FAILED: &str = "admission_failed";
/// 未实现/不支持操作（core-v1 122；400；P1 无上混/下混）。
pub const CODE_UNSUPPORTED: &str = "unsupported";
/// plan 哈希不匹配（core-v1 112；400）。
pub const CODE_HASH_MISMATCH: &str = "hash_mismatch";
/// 容量不足（core-v1 113；400）。
pub const CODE_CAPACITY_EXCEEDED: &str = "capacity_exceeded";
/// quarantine 耗尽（core-v1 121；409）。
pub const CODE_QUARANTINE_EXHAUSTED: &str = "quarantine_exhausted";
/// crossfade 预算拒绝（core-v1 123；409）。
pub const CODE_CROSSFADE_BUDGET: &str = "crossfade_budget";
/// plan ABI/布局拒绝（core-v1 101-103；400）。
pub const CODE_INVALID_PLAN: &str = "invalid_plan";
/// 4.1.2 目标与 P1_412_ROLES/P1_412_CHANNELS 逐项不一致（F-14；Y11 唯一映射；400）。
/// 稳定码：客户端可据此区分「白名单外角色」与「4.1.2 槽位表不符」。
pub const CODE_LAYOUT_ROLES_MISMATCH: &str = "layout_roles_mismatch";
/// 模式切换失败后已回滚（A03 exit 9；F-30：不复用 119 stale；409）。
pub const CODE_ROLLED_BACK: &str = "rolled_back";

/// REST 错误：内部统一表示，序列化时总是输出统一 envelope。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestError {
    /// HTTP 状态码（400/401/403/404/405/409/410/413/429/500/503）。
    pub status: u16,
    /// 机器可读错误码（{"error":{"code"}}）。
    pub code: &'static str,
    /// 人可读说明（不得含 token/密码/密钥/内部绝对路径）。
    pub message: String,
    /// 退役端点的替代路径（仅 X15 410 retired 响应携带；其余 None）。
    pub replacement: Option<String>,
}

impl RestError {
    /// 构造。
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        RestError {
            status,
            code,
            message: message.into(),
            replacement: None,
        }
    }

    /// 400。
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        RestError::new(400, code, message)
    }

    /// 401。
    pub fn unauthorized() -> Self {
        RestError::new(401, CODE_UNAUTHORIZED, "authentication required")
    }

    /// 403。
    pub fn forbidden(message: impl Into<String>) -> Self {
        RestError::new(403, CODE_FORBIDDEN, message)
    }

    /// 404。
    pub fn not_found(message: impl Into<String>) -> Self {
        RestError::new(404, CODE_NOT_FOUND, message)
    }

    /// 405。
    pub fn method_not_allowed(method: &str) -> Self {
        RestError::new(
            405,
            CODE_METHOD_NOT_ALLOWED,
            format!("method {method} not allowed on this path"),
        )
    }

    /// 409。
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        RestError::new(409, code, message)
    }

    /// 410（X15）：envelope 额外带 replacement 字段（X15 明确要求）。
    pub fn retired(replacement: &str) -> Self {
        let mut error = RestError::new(
            410,
            CODE_RETIRED,
            format!("endpoint retired; replacement: {replacement}"),
        );
        error.replacement = Some(replacement.to_string());
        error
    }

    /// 413（X22）。
    pub fn payload_too_large(limit: usize) -> Self {
        RestError::new(
            413,
            CODE_PAYLOAD_TOO_LARGE,
            format!("request body exceeds {limit} bytes"),
        )
    }

    /// 429（R04）。
    pub fn rate_limited(seconds: u64) -> Self {
        RestError::new(
            429,
            CODE_RATE_LIMITED,
            format!("too many failed login attempts; retry after {seconds}s"),
        )
    }

    /// 500（不假成功）。
    pub fn internal(message: impl Into<String>) -> Self {
        RestError::new(500, CODE_INTERNAL_ERROR, message)
    }

    /// 503（R16/R48 后端未接线）。
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        RestError::new(503, CODE_SERVICE_UNAVAILABLE, message)
    }

    /// 统一 envelope（R50/X21）：{"error":{"code","message"}}；X15 退役响应额外带 replacement。
    pub fn to_json(&self) -> JsonValue {
        let mut error = vec![
            ("code".to_string(), JsonValue::str(self.code)),
            ("message".to_string(), JsonValue::str(self.message.as_str())),
        ];
        if let Some(replacement) = &self.replacement {
            error.push((
                "replacement".to_string(),
                JsonValue::str(replacement.as_str()),
            ));
        }
        JsonValue::obj(vec![("error", JsonValue::Obj(error))])
    }
}

/// 把 core-v1 §9.2 RtStatus 映射成 REST 错误码/状态（engine 契约消费；核心映射表）。
///
/// 规则（与 compat R48/R49/X18 对齐）：
/// - 117/118/119/120/121/123 -> 409（冲突类；device/switch/stale/quarantine/crossfade）
/// - control 扩展码 124（模式回滚）-> 409 rolled_back（F-30；不复用 119）
/// - 104/105/110/122 -> 400 unsupported/unsupported_value（显式拒绝，不静默夹取）
/// - 101/102/103/106..109/111..116 -> 400（plan/admission 拒绝）
/// - 201..=299 / 301..=407 -> 500 internal_error（RT 级错误不应由 control 面返回）
///
/// T24（ws.rs）交付后：本函数委托 WsOpError::from_rt_status + ws::rest_equivalent，保证 REST
/// 与 WS 两个面从同一 RtStatus 派生、不出现两套错误码口径（唯一补充：core-v1 123
/// crossfade 预算，T24 映射表未覆盖，这里按 409 crossfade_budget 显式补上）。
pub fn rt_status_to_error(status: RtStatus) -> RestError {
    // F-30：模式回滚用 control 层扩展码 124，不能复用 119（119 只表示 plan stale）；
    // 映射到 409 rolled_back（A03 exit 9 的 REST 稳定码）。
    if status.code() == ws::RT_STATUS_MODE_ROLLED_BACK {
        return RestError::conflict(
            CODE_ROLLED_BACK,
            format!(
                "mode switch failed and was rolled back (rt_status={})",
                status.code()
            ),
        );
    }
    if status.code() == 123 {
        return RestError::conflict(
            CODE_CROSSFADE_BUDGET,
            format!("crossfade budget rejected (rt_status={})", status.code()),
        );
    }
    // R49 明确 118 actual hwparams 冲突为 409 device_params；T24 ws 映射把 118 归到
    // invalid_argument/invalid_field(400)，此处保留 REST 面的 409 语义（与 117/119/120 同族）。
    if status == RtStatus::ERR_DEVICE_PARAMS {
        return RestError::conflict(
            CODE_DEVICE_PARAMS,
            format!(
                "actual device parameters do not satisfy the plan (rt_status={})",
                status.code()
            ),
        );
    }
    let ws_error = ws::WsOpError::from_rt_status(status, "rest control-plane");
    ws_error_to_rest(&ws_error)
}

/// T24 WsOpError -> REST 错误（rest_equivalent 映射 + 状态码）。
pub fn ws_error_to_rest(error: &ws::WsOpError) -> RestError {
    let code = ws::rest_equivalent(error.code()).unwrap_or(CODE_INTERNAL_ERROR);
    let status = match code {
        CODE_PAYLOAD_TOO_LARGE => 413,
        CODE_UNSUPPORTED
        | CODE_INVALID_PLAN
        | CODE_ADMISSION_FAILED
        | CODE_INVALID_JSON
        | CODE_INVALID_FIELD => 400,
        CODE_NOT_PREPARED
        | CODE_STALE
        | CODE_SWITCH_BUSY
        | CODE_DEVICE_BUSY
        | CODE_QUARANTINE_EXHAUSTED
        | CODE_BUSY => 409,
        _ => 500,
    };
    RestError::new(status, code, error.message().to_string())
}

// ---------------------------------------------------------------------------
// 严格 JSON 子集（E-1：无 serde 依赖）
// ---------------------------------------------------------------------------

/// JSON 值（对象保持插入顺序；数字必须是有限 f64）。
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// null。
    Null,
    /// bool。
    Bool(bool),
    /// 有限 f64。
    Num(f64),
    /// string。
    Str(String),
    /// array。
    Arr(Vec<JsonValue>),
    /// object（保序；重复键在解析时拒绝）。
    Obj(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// 构造对象。
    pub fn obj(fields: Vec<(&str, JsonValue)>) -> JsonValue {
        JsonValue::Obj(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// 构造数组。
    pub fn arr(items: Vec<JsonValue>) -> JsonValue {
        JsonValue::Arr(items)
    }

    /// 构造字符串。
    pub fn str(value: impl Into<String>) -> JsonValue {
        JsonValue::Str(value.into())
    }

    /// object 取字段（不存在返回 None；重复键不会出现）。
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Obj(entries) => entries
                .iter()
                .find(|(existing, _)| existing == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// 解析文本（严格：无 NaN/Inf、无重复键、无尾随字节、深度 <= 16）。
    pub fn parse(text: &str) -> Result<JsonValue, JsonParseError> {
        let mut parser = JsonParser {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        };
        parser.skip_ws();
        let value = parser.parse_value(0)?;
        parser.skip_ws();
        if parser.pos != parser.bytes.len() {
            return Err(JsonParseError {
                kind: JsonParseErrorKind::Syntax,
                offset: parser.pos,
            });
        }
        Ok(value)
    }

    /// 序列化（紧凑；非 ASCII 原样 UTF-8）。
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_into(&mut out);
        out
    }

    fn write_into(&self, out: &mut String) {
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(true) => out.push_str("true"),
            JsonValue::Bool(false) => out.push_str("false"),
            JsonValue::Num(value) => out.push_str(&format_json_number(*value)),
            JsonValue::Str(text) => write_json_string(out, text),
            JsonValue::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write_into(out);
                }
                out.push(']');
            }
            JsonValue::Obj(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_json_string(out, key);
                    out.push(':');
                    value.write_into(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> Self {
        JsonValue::Str(value.to_string())
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> Self {
        JsonValue::Str(value)
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> Self {
        JsonValue::Bool(value)
    }
}

impl From<f64> for JsonValue {
    fn from(value: f64) -> Self {
        JsonValue::Num(value)
    }
}

impl From<u64> for JsonValue {
    fn from(value: u64) -> Self {
        JsonValue::Num(value as f64)
    }
}

impl From<u32> for JsonValue {
    fn from(value: u32) -> Self {
        JsonValue::Num(f64::from(value))
    }
}

impl From<u16> for JsonValue {
    fn from(value: u16) -> Self {
        JsonValue::Num(f64::from(value))
    }
}

impl From<usize> for JsonValue {
    fn from(value: usize) -> Self {
        JsonValue::Num(value as f64)
    }
}

impl From<i64> for JsonValue {
    fn from(value: i64) -> Self {
        JsonValue::Num(value as f64)
    }
}

/// JSON 解析错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonParseErrorKind {
    /// 语法错误。
    Syntax,
    /// 重复键（X02 严格）。
    DuplicateKey,
    /// 非有限数字（NaN/Inf/溢出）。
    NonFiniteNumber,
    /// 嵌套过深。
    TooDeep,
}

/// JSON 解析错误（带字节偏移，便于诊断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonParseError {
    /// 错误类别。
    pub kind: JsonParseErrorKind,
    /// 字节偏移。
    pub offset: usize,
}

struct JsonParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn error(&self, kind: JsonParseErrorKind) -> JsonParseError {
        JsonParseError {
            kind,
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, JsonParseError> {
        if depth > 16 {
            return Err(self.error(JsonParseErrorKind::TooDeep));
        }
        match self.peek() {
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => Ok(JsonValue::Str(self.parse_string()?)),
            Some(b't') => {
                self.expect_literal("true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal("false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'n') => {
                self.expect_literal("null")?;
                Ok(JsonValue::Null)
            }
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            _ => Err(self.error(JsonParseErrorKind::Syntax)),
        }
    }

    fn expect_literal(&mut self, literal: &str) -> Result<(), JsonParseError> {
        if self.text[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.error(JsonParseErrorKind::Syntax))
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue, JsonParseError> {
        self.pos += 1; // '{'
        self.skip_ws();
        let mut entries: Vec<(String, JsonValue)> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Obj(entries));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error(JsonParseErrorKind::Syntax));
            }
            let key = self.parse_string()?;
            if entries.iter().any(|(existing, _)| existing == &key) {
                return Err(self.error(JsonParseErrorKind::DuplicateKey));
            }
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error(JsonParseErrorKind::Syntax));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.parse_value(depth + 1)?;
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Obj(entries));
                }
                _ => return Err(self.error(JsonParseErrorKind::Syntax)),
            }
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue, JsonParseError> {
        self.pos += 1; // '['
        self.skip_ws();
        let mut items: Vec<JsonValue> = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Arr(items));
                }
                _ => return Err(self.error(JsonParseErrorKind::Syntax)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonParseError> {
        self.pos += 1; // '"'
        let mut out = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error(JsonParseErrorKind::Syntax));
            }
            let ch = self.current_char()?;
            match ch {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    self.pos += 1;
                    out.push(self.parse_escape()?);
                }
                c if (c as u32) < 0x20 => return Err(self.error(JsonParseErrorKind::Syntax)),
                c => {
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn current_char(&self) -> Result<char, JsonParseError> {
        self.text[self.pos..]
            .chars()
            .next()
            .ok_or_else(|| self.error(JsonParseErrorKind::Syntax))
    }

    fn parse_hex4(&mut self) -> Result<u32, JsonParseError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.error(JsonParseErrorKind::Syntax));
        }
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = (self.bytes[self.pos] as char)
                .to_digit(16)
                .ok_or_else(|| self.error(JsonParseErrorKind::Syntax))?;
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }

    fn parse_escape(&mut self) -> Result<char, JsonParseError> {
        if self.pos >= self.bytes.len() {
            return Err(self.error(JsonParseErrorKind::Syntax));
        }
        let escape = self.bytes[self.pos];
        self.pos += 1;
        match escape {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'b' => Ok('\u{8}'),
            b'f' => Ok('\u{c}'),
            b'n' => Ok('\n'),
            b'r' => Ok('\r'),
            b't' => Ok('\t'),
            b'u' => {
                let first = self.parse_hex4()?;
                if (0xD800..=0xDBFF).contains(&first) {
                    if self.peek() != Some(b'\\') {
                        return Err(self.error(JsonParseErrorKind::Syntax));
                    }
                    self.pos += 1;
                    if self.peek() != Some(b'u') {
                        return Err(self.error(JsonParseErrorKind::Syntax));
                    }
                    self.pos += 1;
                    let second = self.parse_hex4()?;
                    if !(0xDC00..=0xDFFF).contains(&second) {
                        return Err(self.error(JsonParseErrorKind::Syntax));
                    }
                    let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                    char::from_u32(combined).ok_or_else(|| self.error(JsonParseErrorKind::Syntax))
                } else if (0xDC00..=0xDFFF).contains(&first) {
                    Err(self.error(JsonParseErrorKind::Syntax))
                } else {
                    char::from_u32(first).ok_or_else(|| self.error(JsonParseErrorKind::Syntax))
                }
            }
            _ => Err(self.error(JsonParseErrorKind::Syntax)),
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error(JsonParseErrorKind::Syntax));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.error(JsonParseErrorKind::Syntax)),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error(JsonParseErrorKind::Syntax));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error(JsonParseErrorKind::Syntax));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = &self.text[start..self.pos];
        let value: f64 = text
            .parse()
            .map_err(|_| self.error(JsonParseErrorKind::Syntax))?;
        if !value.is_finite() {
            return Err(self.error(JsonParseErrorKind::NonFiniteNumber));
        }
        Ok(JsonValue::Num(value))
    }
}

fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn format_json_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

// ---------------------------------------------------------------------------
// SHA-256 / HMAC-SHA256 / PBKDF2（E-2：无第三方 crypto 依赖）
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = Vec::with_capacity(data.len() + 72);
    message.extend_from_slice(data);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut out = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    for byte in block.iter() {
        inner.push(byte ^ 0x36);
    }
    inner.extend_from_slice(message);
    let inner_hash = sha256(&inner);
    let mut outer = Vec::with_capacity(64 + 32);
    for byte in block.iter() {
        outer.push(byte ^ 0x5c);
    }
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, out_len: usize) -> Vec<u8> {
    let iterations = iterations.max(1);
    let mut out = Vec::with_capacity(out_len);
    let mut block_index: u32 = 1;
    while out.len() < out_len {
        let mut indexed = Vec::with_capacity(salt.len() + 4);
        indexed.extend_from_slice(salt);
        indexed.extend_from_slice(&block_index.to_be_bytes());
        let mut u = hmac_sha256(password, &indexed);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (left, right) in t.iter_mut().zip(u.iter()) {
                *left ^= *right;
            }
        }
        out.extend_from_slice(&t);
        block_index += 1;
    }
    out.truncate(out_len);
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn is_lower_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

static RANDOM_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn random_16() -> [u8; 16] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let counter = RANDOM_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut first = RandomState::new().build_hasher();
    first.write_u64(counter);
    first.write_u64(now_nanos());
    let first_value = first.finish();
    let mut second = RandomState::new().build_hasher();
    second.write_u64(first_value);
    second.write_u64(counter ^ 0x9e37_79b9_7f4a_7c15);
    second.write_u64(now_unix());
    let second_value = second.finish();
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&first_value.to_be_bytes());
    out[8..].copy_from_slice(&second_value.to_be_bytes());
    out
}

fn random_hex_16() -> String {
    hex_lower(&random_16())
}

// ---------------------------------------------------------------------------
// HTTP 传输无关类型（T70/T26 传输层直接映射）
// ---------------------------------------------------------------------------

/// HTTP 方法（G-1 传输层解析后传入；未知方法由传输层 400/405）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// GET。
    Get,
    /// POST。
    Post,
    /// PUT。
    Put,
    /// DELETE。
    Delete,
}

impl Method {
    /// 大写字面（Allow/日志）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }

    /// 解析（大小写不敏感；未知方法返回 None，由传输层拒绝）。
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_uppercase().as_str() {
            "GET" => Some(Method::Get),
            "POST" => Some(Method::Post),
            "PUT" => Some(Method::Put),
            "DELETE" => Some(Method::Delete),
            _ => None,
        }
    }

    /// 是否写方法（CSRF 检查范围）。
    pub const fn is_write(self) -> bool {
        !matches!(self, Method::Get)
    }
}

/// 传输无关 REST 请求。路径必须已由传输层解码（不含 query/fragment）；query 保留原样。
#[derive(Debug, Clone)]
pub struct RestRequest {
    /// 方法。
    pub method: Method,
    /// 路径（以 / 开头；不含 query）。
    pub path: String,
    /// query 键值（本模块目前不消费；保留给 T70/未来端点）。
    pub query: Vec<(String, String)>,
    /// 请求头（名称原样；查找大小写不敏感）。
    pub headers: Vec<(String, String)>,
    /// 请求体原始字节（JSON 端点要求 UTF-8 JSON 对象）。
    pub body: Vec<u8>,
}

impl RestRequest {
    /// 构造。
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        RestRequest {
            method,
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// GET。
    pub fn get(path: impl Into<String>) -> Self {
        RestRequest::new(Method::Get, path)
    }

    /// POST。
    pub fn post(path: impl Into<String>) -> Self {
        RestRequest::new(Method::Post, path)
    }

    /// PUT。
    pub fn put(path: impl Into<String>) -> Self {
        RestRequest::new(Method::Put, path)
    }

    /// DELETE。
    pub fn delete(path: impl Into<String>) -> Self {
        RestRequest::new(Method::Delete, path)
    }

    /// 追加请求头（builder）。
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// 设置 JSON 请求体（builder）。
    pub fn json_body(mut self, json: &str) -> Self {
        self.headers
            .push(("Content-Type".to_string(), "application/json".to_string()));
        self.body = json.as_bytes().to_vec();
        self
    }

    /// 设置原始请求体（builder）。
    pub fn body_bytes(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// 追加 query（builder；本模块目前不消费）。
    pub fn query_param(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_string(), value.to_string()));
        self
    }

    /// 请求头查找（名称大小写不敏感；重复头取第一个）。
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Cookie 查找（Cookie 头按 name=value; 解析；不会 URL 解码）。
    pub fn cookie_value(&self, name: &str) -> Option<&str> {
        let raw = self.header_value("Cookie")?;
        for part in raw.split(';') {
            let part = part.trim();
            let (key, value) = part.split_once('=')?;
            if key.trim() == name {
                return Some(value.trim());
            }
        }
        None
    }
}

/// 传输无关 REST 响应。
#[derive(Debug, Clone)]
pub struct RestResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头（含 Security/API version/Cookie 等）。
    pub headers: Vec<(String, String)>,
    /// 响应体（JSON 或纯文本）。
    pub body: String,
}

impl RestResponse {
    /// 空响应构造。
    pub fn new(status: u16) -> Self {
        let mut response = RestResponse {
            status,
            headers: Vec::new(),
            body: String::new(),
        };
        for (name, value) in SECURITY_HEADERS {
            response.headers.push((name.to_string(), value.to_string()));
        }
        response.headers.push((
            API_VERSION_HEADER.to_string(),
            API_VERSION_RESPONSE_VALUE.to_string(),
        ));
        response
    }

    /// JSON 响应（Content-Type: application/json; charset=utf-8）。
    pub fn json(status: u16, value: &JsonValue) -> Self {
        let mut response = RestResponse::new(status);
        response.headers.push((
            "Content-Type".to_string(),
            "application/json; charset=utf-8".to_string(),
        ));
        response.body = value.to_json();
        response
    }

    /// 纯文本响应。
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        let mut response = RestResponse::new(status);
        response.headers.push((
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        ));
        response.body = body.into();
        response
    }

    /// 追加/覆盖响应头（builder）。
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// 响应头查找（大小写不敏感）。
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 错误 -> 统一 envelope 响应（R50/X21）。
    pub fn from_error(error: &RestError) -> Self {
        RestResponse::json(error.status, &error.to_json())
    }
}

// ---------------------------------------------------------------------------
// 域类型：mode / target / coeff / config / engine 契约（R10/R13/R49/R48）
// ---------------------------------------------------------------------------

/// P1 模式（A01；大小写不敏感解析，canonical 小写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    /// 音频关闭。
    Off,
    /// Toslink 2.0 stereo strict 直出（FL/FR，其余静音）。
    Toslink,
    /// 目标多声道（按 target layout 直通映射 + 校准）。
    TargetMultichannel,
    /// AES67 端点模式。
    Aes67,
    /// Dante/Inferno 端点模式。
    Dante,
}

impl ControlMode {
    /// canonical REST 名（A01 小写）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ControlMode::Off => "off",
            ControlMode::Toslink => "toslink",
            ControlMode::TargetMultichannel => "target_multichannel",
            ControlMode::Aes67 => "aes67",
            ControlMode::Dante => "dante",
        }
    }

    /// UI 标签（R10 mode_label）。
    pub const fn label(self) -> &'static str {
        match self {
            ControlMode::Off => "Off",
            ControlMode::Toslink => "Toslink 2.0",
            ControlMode::TargetMultichannel => "Target multichannel",
            ControlMode::Aes67 => "AES67",
            ControlMode::Dante => "Dante",
        }
    }

    /// 解析（大小写不敏感；未知值 None -> 400 unknown_mode，不默认回 off）。
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "off" => Some(ControlMode::Off),
            "toslink" => Some(ControlMode::Toslink),
            "target_multichannel" => Some(ControlMode::TargetMultichannel),
            "aes67" => Some(ControlMode::Aes67),
            "dante" => Some(ControlMode::Dante),
            _ => None,
        }
    }

    /// 是否播放模式（target 布局有意义）。
    pub const fn is_playback(self) -> bool {
        matches!(
            self,
            ControlMode::Toslink
                | ControlMode::TargetMultichannel
                | ControlMode::Aes67
                | ControlMode::Dante
        )
    }
}

/// 输出角色 -> DAC 通道（0-based；由 T57 overlay 解析后送入，REST 只校验一致性）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleDac {
    /// 角色名（ALLOWED_ROLES；大写）。
    pub role: String,
    /// DAC 通道号（0-based，< channels）。
    pub dac: u8,
}

/// 目标布局（R49 GET/PUT /api/runtime/target；Y10/Y11）。
///
/// 说明：唯一 role->DAC 来源是板端 overlay layout-presets（T57 解析并绑定 hash）；
/// 本类型承载已解析结果，REST 只做一致性/白名单校验，绝不硬编码覆盖 T57 的解析。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSpec {
    /// 布局名（如 "4.1.2"）。
    pub layout: String,
    /// 输出容器宽度（实际设备通道数由 engine/T57 校验）。
    pub channels: u16,
    /// 角色->DAC 表（显式，非硬编码表驱动）。
    pub roles: Vec<RoleDac>,
    /// 低带 dense 参与者->DAC（0..4 -> DAC 0/1/2/3/7 由 T57 给；可为空）。
    pub low_band_dacs: Vec<u8>,
    /// profile 标识（overlay/physical profile 名；T57 hash 绑定）。
    pub profile: String,
}

impl TargetSpec {
    /// P1 4.1.2 参考目标（WBS §3/§11 冻结；engine 仍须用实际设备宽度校验）。
    pub fn p1_412() -> Self {
        TargetSpec {
            layout: P1_412_LAYOUT.to_string(),
            channels: P1_412_CHANNELS,
            roles: P1_412_ROLES
                .iter()
                .map(|(role, dac)| RoleDac {
                    role: role.to_string(),
                    dac: *dac,
                })
                .collect(),
            low_band_dacs: P1_412_LOW_BAND_DACS.to_vec(),
            profile: "p1-412".to_string(),
        }
    }

    /// 校验（R49/Y05/Y11/X19）：白名单角色、唯一 role/dac、dac < channels、channels 有效；
    /// layout=4.1.2 时另按 P1_412 表逐项核对（F-14：SUB1 只能槽 7，C/SUB2/SUB3 不属于 4.1.2）。
    pub fn validate(&self) -> Result<(), RestError> {
        if self.layout.trim().is_empty() || self.layout.len() > MAX_LAYOUT_CHARS {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("target.layout must be 1..={MAX_LAYOUT_CHARS} chars"),
            ));
        }
        if !self
            .layout
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "target.layout contains invalid characters",
            ));
        }
        if self.profile.trim().is_empty() || self.profile.len() > MAX_VERSION_CHARS {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("target.profile must be 1..={MAX_VERSION_CHARS} chars"),
            ));
        }
        if !self
            .profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "target.profile contains invalid characters",
            ));
        }
        if self.channels < 2 || usize::from(self.channels) > MAX_OUTPUT_CHANNELS {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("target.channels must be 2..={MAX_OUTPUT_CHANNELS}"),
            ));
        }
        if self.roles.is_empty() || self.roles.len() > MAX_OUTPUT_CHANNELS {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("target.roles must contain 1..={MAX_OUTPUT_CHANNELS} entries"),
            ));
        }
        let mut seen_roles: Vec<&str> = Vec::new();
        let mut seen_dacs: Vec<u8> = Vec::new();
        for entry in &self.roles {
            if !ALLOWED_ROLES.contains(&entry.role.as_str()) {
                return Err(RestError::bad_request(
                    CODE_UNKNOWN_ROLE,
                    format!(
                        "unknown output role '{}'; allowed: {}",
                        entry.role,
                        ALLOWED_ROLES.join(",")
                    ),
                ));
            }
            if seen_roles.contains(&entry.role.as_str()) {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    format!("duplicate role '{}'", entry.role),
                ));
            }
            seen_roles.push(entry.role.as_str());
            if u16::from(entry.dac) >= self.channels {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    format!(
                        "role '{}' DAC {} exceeds target.channels {}",
                        entry.role, entry.dac, self.channels
                    ),
                ));
            }
            if seen_dacs.contains(&entry.dac) {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    format!("duplicate DAC {} in target.roles", entry.dac),
                ));
            }
            seen_dacs.push(entry.dac);
        }
        if self.low_band_dacs.len() > usize::from(self.channels) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "target.low_band_dacs has more entries than channels",
            ));
        }
        let mut seen_low: Vec<u8> = Vec::new();
        for dac in &self.low_band_dacs {
            if u16::from(*dac) >= self.channels {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    format!(
                        "target.low_band_dacs entry {dac} exceeds target.channels {}",
                        self.channels
                    ),
                ));
            }
            if seen_low.contains(dac) {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    format!("duplicate DAC {dac} in target.low_band_dacs"),
                ));
            }
            seen_low.push(*dac);
        }
        self.validate_p1_412_table()?;
        Ok(())
    }

    /// F-14：layout=4.1.2 时角色/槽位必须与 P1_412_ROLES/P1_412_CHANNELS 逐项一致。
    ///
    /// 依据 Y11/WBS §3：4.1.2 只有 7 个物理角色（FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7），
    /// ch6 空、容器 8 槽。`ALLOWED_ROLES` 是跨布局白名单（含 C/SUB2/SUB3），
    /// 不能作为 4.1.2 的充分条件；否则 channels>=10 时 SUB1 可被放到槽 9。
    fn validate_p1_412_table(&self) -> Result<(), RestError> {
        if self.layout != P1_412_LAYOUT {
            return Ok(());
        }
        if self.channels != P1_412_CHANNELS {
            return Err(RestError::bad_request(
                CODE_LAYOUT_ROLES_MISMATCH,
                format!(
                    "layout {} requires channels={} (8-slot container, ch6 empty), got {}",
                    P1_412_LAYOUT, P1_412_CHANNELS, self.channels
                ),
            ));
        }
        if self.roles.len() != P1_412_ROLES.len() {
            return Err(RestError::bad_request(
                CODE_LAYOUT_ROLES_MISMATCH,
                format!(
                    "layout {} requires exactly {} roles ({}), got {} roles",
                    P1_412_LAYOUT,
                    P1_412_ROLES.len(),
                    P1_412_ROLES
                        .iter()
                        .map(|(role, dac)| format!("{role}={dac}"))
                        .collect::<Vec<_>>()
                        .join(","),
                    self.roles.len()
                ),
            ));
        }
        for (role, dac) in P1_412_ROLES {
            let matched = self
                .roles
                .iter()
                .find(|entry| entry.role == role)
                .ok_or_else(|| {
                    RestError::bad_request(
                        CODE_LAYOUT_ROLES_MISMATCH,
                        format!(
                            "layout {} requires role {role} at DAC {dac}; role is missing",
                            P1_412_LAYOUT
                        ),
                    )
                })?;
            if matched.dac != dac {
                return Err(RestError::bad_request(
                    CODE_LAYOUT_ROLES_MISMATCH,
                    format!(
                        "layout {} requires {role} at DAC {dac} (Y11 unique mapping), got DAC {}",
                        P1_412_LAYOUT, matched.dac
                    ),
                ));
            }
        }
        if !self.low_band_dacs.is_empty() && self.low_band_dacs != P1_412_LOW_BAND_DACS {
            return Err(RestError::bad_request(
                CODE_LAYOUT_ROLES_MISMATCH,
                format!(
                    "layout {} requires low_band_dacs={:?} (dense 0..4), got {:?}",
                    P1_412_LAYOUT, P1_412_LOW_BAND_DACS, self.low_band_dacs
                ),
            ));
        }
        Ok(())
    }

    /// 规范化文本（hash/fingerprint/prepare 判等用；顺序即输入顺序）。
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        out.push_str("layout=");
        out.push_str(&self.layout);
        out.push_str(";channels=");
        out.push_str(&self.channels.to_string());
        out.push_str(";profile=");
        out.push_str(&self.profile);
        out.push_str(";roles=");
        for entry in &self.roles {
            out.push_str(&entry.role);
            out.push(':');
            out.push_str(&entry.dac.to_string());
            out.push(',');
        }
        out.push_str(";low_band=");
        for dac in &self.low_band_dacs {
            out.push_str(&dac.to_string());
            out.push(',');
        }
        out
    }

    /// 目标指纹（sha256 hex；engine 用作 target_hash，T57 可绑定 overlay hash）。
    pub fn fingerprint(&self) -> String {
        hex_lower(&sha256(self.canonical().as_bytes()))
    }

    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        let roles = self
            .roles
            .iter()
            .map(|entry| {
                JsonValue::obj(vec![
                    ("role", JsonValue::str(entry.role.as_str())),
                    ("dac", JsonValue::from(entry.dac)),
                ])
            })
            .collect();
        let low = self
            .low_band_dacs
            .iter()
            .map(|dac| JsonValue::from(*dac))
            .collect();
        JsonValue::obj(vec![
            ("layout", JsonValue::str(self.layout.as_str())),
            ("channels", JsonValue::from(self.channels)),
            ("profile", JsonValue::str(self.profile.as_str())),
            ("roles", JsonValue::Arr(roles)),
            ("low_band_dacs", JsonValue::Arr(low)),
            ("target_hash", JsonValue::str(self.fingerprint())),
        ])
    }

    /// 从 JSON 解析（严格字段白名单，X02）。
    pub fn from_json(value: &JsonValue, path: &'static str) -> Result<Self, RestError> {
        let entries = object_entries(value, path)?;
        let mut fields = Fields::new(entries, path);
        let layout = fields.take_str("layout")?;
        let channels = fields.take_u16("channels")?;
        let profile = fields.take_str("profile")?;
        let roles_value = fields.take_required("roles")?;
        let low_value = fields.take_opt("low_band_dacs");
        fields.finish()?;
        let roles = parse_role_list(&roles_value, "target.roles")?;
        let low_band_dacs = match low_value {
            None | Some(JsonValue::Null) => Vec::new(),
            Some(value) => parse_u8_list(&value, "target.low_band_dacs")?,
        };
        let spec = TargetSpec {
            layout,
            channels,
            roles,
            low_band_dacs,
            profile,
        };
        spec.validate()?;
        Ok(spec)
    }
}

/// coeff 版本引用（Y24/T13 IR：version + sha256；stale/hash 校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoeffRef {
    /// 版本名（[A-Za-z0-9._-]，1..=64）。
    pub version: String,
    /// 系数内容/artifact sha256（64 位小写 hex）。
    pub sha256: String,
}

impl CoeffRef {
    /// 构造并校验。
    pub fn new(version: impl Into<String>, sha256: impl Into<String>) -> Result<Self, RestError> {
        let value = CoeffRef {
            version: version.into(),
            sha256: sha256.into(),
        };
        value.validate()?;
        Ok(value)
    }

    /// 校验（X18/Y24）。
    pub fn validate(&self) -> Result<(), RestError> {
        if self.version.is_empty() || self.version.len() > MAX_VERSION_CHARS {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("coeff.version must be 1..={MAX_VERSION_CHARS} chars"),
            ));
        }
        if !self
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "coeff.version contains invalid characters",
            ));
        }
        if !is_lower_hex64(&self.sha256) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "coeff.sha256 must be 64 lowercase hex chars",
            ));
        }
        Ok(())
    }

    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("version", JsonValue::str(self.version.as_str())),
            ("sha256", JsonValue::str(self.sha256.as_str())),
        ])
    }

    /// 从 JSON 解析（严格字段）。
    pub fn from_json(value: &JsonValue, path: &'static str) -> Result<Self, RestError> {
        let entries = object_entries(value, path)?;
        let mut fields = Fields::new(entries, path);
        let version = fields.take_str("version")?;
        let hash = fields.take_str("sha256")?;
        fields.finish()?;
        CoeffRef::new(version, hash)
    }
}

fn coeff_option_json(coeff: &Option<CoeffRef>) -> JsonValue {
    match coeff {
        Some(value) => value.to_json(),
        None => JsonValue::Null,
    }
}

/// 期望配置（desired；R49 GET/PUT /api/runtime/config）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// 模式。
    pub mode: ControlMode,
    /// 采样率（48k/96k）。
    pub sample_rate: u32,
    /// chunksize（runtime 仅 64/128；T02 Q-11）。
    pub chunksize: u16,
    /// 目标布局/target。
    pub target: TargetSpec,
    /// 校准系数版本（None = 基线/identity P）。
    pub coeff: Option<CoeffRef>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            mode: ControlMode::Toslink,
            sample_rate: DEFAULT_SAMPLE_RATE,
            chunksize: DEFAULT_CHUNKSIZE,
            target: TargetSpec::p1_412(),
            coeff: None,
        }
    }
}

impl RuntimeConfig {
    /// 校验（R13/R14/R49；显式拒绝、不静默夹取）。
    pub fn validate(&self) -> Result<(), RestError> {
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                format!("sample_rate must be one of {:?}", SAMPLE_RATES_HZ),
            ));
        }
        if !BLOCK_SIZES.contains(&self.chunksize) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                format!("chunksize must be one of {:?} (runtime v1; T02 Q-11); 32/256/512/1024 are UI reference only", BLOCK_SIZES),
            ));
        }
        self.target.validate()?;
        if let Some(coeff) = &self.coeff {
            coeff.validate()?;
        }
        Ok(())
    }

    /// 规范化文本（prepare 判等；与 target fingerprint 一起绑定）。
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        out.push_str("mode=");
        out.push_str(self.mode.as_str());
        out.push_str(";sample_rate=");
        out.push_str(&self.sample_rate.to_string());
        out.push_str(";chunksize=");
        out.push_str(&self.chunksize.to_string());
        out.push_str(";target=");
        out.push_str(&self.target.canonical());
        out.push_str(";coeff=");
        match &self.coeff {
            Some(coeff) => {
                out.push_str(&coeff.version);
                out.push(':');
                out.push_str(&coeff.sha256);
            }
            None => out.push_str("none"),
        }
        out
    }

    /// 配置指纹（sha256 hex；prepare 后配置变化 -> 409 stale）。
    pub fn fingerprint(&self) -> String {
        hex_lower(&sha256(self.canonical().as_bytes()))
    }

    /// JSON 投影（不含 actual；actual 由 engine snapshot 提供）。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("mode", JsonValue::str(self.mode.as_str())),
            ("sample_rate", JsonValue::from(self.sample_rate)),
            ("chunksize", JsonValue::from(self.chunksize)),
            ("target", self.target.to_json()),
            ("coeff", coeff_option_json(&self.coeff)),
            ("config_hash", JsonValue::str(self.fingerprint())),
        ])
    }
}

/// engine 实际状态快照（唯一 actual 来源；REST 不伪造）。
#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    /// 运行时状态（Running/Draining/SafetyMute/Faulted/Stopped/Init/Admitted/...）。
    pub runtime_state: String,
    /// 设备 owner（idle/playback/calibration/transition）。
    pub device_owner: String,
    /// 当前 epoch（core-v1 §5.2）。
    pub epoch: u64,
    /// 实际生效 config_version（PlanVersion 字符串化；T26/T20 提供）。
    pub actual_version: String,
    /// 实际 graph_id（64 位小写 hex；core-v1 §5.3/T20）。
    pub graph_id: String,
    /// 实际生效 coeff。
    pub coeff: Option<CoeffRef>,
    /// last-good coeff（R48/P12 回滚目标）。
    pub last_good_coeff: Option<CoeffRef>,
    /// 可用 coeff 版本列表。
    pub available_coeffs: Vec<CoeffRef>,
    /// 实际 target hash（T57 overlay 解析 hash）。
    pub target_hash: String,
    /// 实际 target。
    pub target: TargetSpec,
    /// 实际模式。
    pub mode: ControlMode,
    /// 实际 mode profile（A08/A09）。
    pub mode_profile: Option<String>,
    /// 实际采样率（actual hwparams，不是配置意图）。
    pub sample_rate: u32,
    /// 实际 chunksize/period frames。
    pub chunksize: u16,
    /// 实际音量 dB。
    pub volume_db: f64,
    /// 实际 mute。
    pub mute: bool,
    /// 累计 xrun 计数（P02）。
    pub xruns: u64,
    /// 累计 deadline miss（P02）。
    pub deadline_misses: u64,
    /// 累计 clip 样本（P02）。
    pub clipped_samples: u64,
    /// 校准是否持有设备（R48 独占冲突）。
    pub calibration_owner: bool,
    /// 最近提交时间（unix）。
    pub committed_unix: u64,
}

/// prepare 结果（engine admit 后返回；不改输出）。
#[derive(Debug, Clone)]
pub struct PrepareOutcome {
    /// engine 内部 prepared token（commit 时回传）。
    pub engine_token: u64,
    /// planned config version（未生效；显示用）。
    pub planned_version: String,
    /// planned target hash。
    pub planned_target_hash: String,
    /// planned coeff。
    pub planned_coeff: Option<CoeffRef>,
    /// admission 摘要（人读；有界）。
    pub admission: String,
    /// T24 同口径 prepare 凭据（REST/WS 同一套校验；validate_commit_ack 的 prepared 侧）。
    pub prepared: ws::PreparedConfig,
}

/// commit 成功后的 actual ack（core-v1 §5.3：plan_version 已在完整 block + 4 worker ack 后才允许）。
#[derive(Debug, Clone)]
pub struct CommitOutcome {
    /// 实际生效 config version（显示用；必须与 ack.plan_version 一致）。
    pub actual_version: String,
    /// 实际 coeff。
    pub coeff: Option<CoeffRef>,
    /// 实际 target hash。
    pub target_hash: String,
    /// 提交后 epoch。
    pub epoch: u64,
    /// 提交时间（unix）。
    pub committed_unix: u64,
    /// T24 同口径 commit ack 证据（blocks/ack_mask/version/graph/config/rolled_back；
    /// REST 必须用 ws::validate_commit_ack 核对后才允许返回 200）。
    pub ack: ws::CommitAck,
}

/// volume 写入后的 actual ack。
#[derive(Debug, Clone, Copy)]
pub struct VolumeAck {
    /// 实际 dB。
    pub volume_db: f64,
    /// 实际 mute。
    pub mute: bool,
}

/// mute 写入后的 actual ack。
#[derive(Debug, Clone, Copy)]
pub struct MuteAck {
    /// 实际 mute。
    pub mute: bool,
    /// 实际 dB。
    pub volume_db: f64,
}

/// 模式切换结果（R11：202 只表示接受；switch_state 区分已切/在途）。
#[derive(Debug, Clone)]
pub struct ModeAck {
    /// 实际/目标模式。
    pub mode: ControlMode,
    /// 实际 profile。
    pub profile: Option<String>,
    /// 实际 target hash。
    pub target_hash: String,
    /// 实际采样率。
    pub sample_rate: u32,
    /// 实际 chunksize。
    pub chunksize: u16,
    /// 切换状态（switched/switching/pending）。
    pub switch_state: String,
}

/// 校准 job REST 投影（T60 JobSnapshot 的只读子集；状态机归 T60）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalJobView {
    /// job id（T60 JobId Display，如 job-0001）。
    pub job_id: String,
    /// 类型（full/retest）。
    pub kind: String,
    /// 状态（CAL_JOB_STATES；active 时为 stage 名）。
    pub state: String,
    /// active stage（终态/queued 为 None）。
    pub stage: Option<String>,
    /// 进度 0..1（单调；终态 1.0）。
    pub progress: f64,
    /// apply 实际版本（apply 后必须有，否则 REST 500 不假 ack）。
    pub applied_version: Option<String>,
    /// last-good 版本（rollback 目标）。
    pub last_good_version: Option<String>,
    /// 失败原因（无内部路径）。
    pub failure: Option<String>,
    /// 创建时间。
    pub created_unix: u64,
    /// 更新时间。
    pub updated_unix: u64,
}

impl CalJobView {
    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("job_id", JsonValue::str(self.job_id.as_str())),
            ("kind", JsonValue::str(self.kind.as_str())),
            ("state", JsonValue::str(self.state.as_str())),
            (
                "stage",
                match &self.stage {
                    Some(stage) => JsonValue::str(stage.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("progress", JsonValue::from(self.progress)),
            (
                "applied_version",
                match &self.applied_version {
                    Some(value) => JsonValue::str(value.as_str()),
                    None => JsonValue::Null,
                },
            ),
            (
                "last_good_version",
                match &self.last_good_version {
                    Some(value) => JsonValue::str(value.as_str()),
                    None => JsonValue::Null,
                },
            ),
            (
                "failure",
                match &self.failure {
                    Some(value) => JsonValue::str(value.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("created_unix", JsonValue::from(self.created_unix)),
            ("updated_unix", JsonValue::from(self.updated_unix)),
        ])
    }

    /// 是否终态。
    pub fn is_terminal(&self) -> bool {
        CAL_JOB_TERMINAL_STATES.contains(&self.state.as_str())
    }
}

/// 校准 job 启动请求（R48；kind 已由 REST 校验）。
#[derive(Debug, Clone)]
pub struct CalStartRequest {
    /// full/retest。
    pub kind: String,
    /// 人读 label。
    pub label: Option<String>,
    /// 完成后自动 apply（默认 false，等显式 apply 端点）。
    pub auto_apply: bool,
    /// 可闻扫频授权（P08；REST start 可选字段）。离线合成 backend 不发声所以不强制；
    /// 真实测量 backend 接线后必须为 true 否则拒绝 start（不得静默放行可闻扫频）。
    pub audible_sweep_authorized: bool,
    /// emperor 的步骤：measure / solve / remeasure。缺省是 enter。
    pub stage: Option<String>,
    /// 支援频段上沿，缺省 150。
    pub f_hi: Option<f64>,
    /// 从喇叭名字，缺省空。
    pub supports: Vec<String>,
    /// flat 或 tilt10。
    pub target: Option<String>,
    /// 校准设置（mic/cal_file/orientation/target_mode/bass）；缺省用 daemon 已保存设置。
    pub settings: Option<calibration_contract::CalibrationSettings>,
    /// level/delay 的 lane 列表；缺省 4.1.2 七角色。
    pub lanes: Vec<String>,
    /// level/delay 参考 lane；缺省 FL。
    pub reference_lane: Option<String>,
    /// level 目标读数 dBFS；缺省 -12。
    pub target_dbfs: Option<f64>,
}

/// 校准 port 错误（区分 404 与 engine 拒绝；Unavailable 带人读原因 -> 503）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalPortError {
    /// job 不存在（404）。
    NotFound,
    /// 拒绝（RtStatus 映射 409/400）。
    Rejected(RtStatus),
    /// port 后端不可用（503；例如 backend 构造失败/apply 回执缺失/等待超时），
    /// 显式失败而不是伪造 job/ack。
    Unavailable(String),
    /// host 字段尚未落地（501 pending_host_field；不伪造 lanes/ack）。
    PendingHostField(String),
}

/// 校准 job port 的身份/接线事实（与 RuntimeEngine::engine_info 同口径的诚实标记）。
///
/// `synthetic=true` 表示 job 状态机可能是真实的，但**测量输入/apply all-ack 是离线合成
/// 模型**（没有真实声卡/4-worker actual）；REST 响应必须原样带出，页面不得显示成
/// 板端 actual。只有接上真实设备测量与真实 host all-ack 的实现才允许 synthetic=false。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalEngineInfo {
    /// port 实现名（例如 "t60-jobmanager-synthetic"）。
    pub engine: &'static str,
    /// true = 测量/设计输入/apply ack 来自离线合成模型（禁止标 actual）。
    pub synthetic: bool,
    /// backend 组成说明（人读；证据/页面用）。
    pub backend: &'static str,
    /// true = 真实声卡/板端音频路径参与（离线 = false）。
    pub real_audio: bool,
    /// 口径备注（有界；例如板端 NOT_RUN 说明）。
    pub note: &'static str,
}

impl CalEngineInfo {
    /// 未接线/未知实现（保守：按 synthetic 处理，绝不冒充 actual）。
    pub const fn unknown() -> Self {
        CalEngineInfo {
            engine: "unknown",
            synthetic: true,
            backend: "not_wired",
            real_audio: false,
            note: "calibration port did not declare engine identity; treated as synthetic",
        }
    }

    /// 内存测试替身（T42 MemoryCalibration/测试 fake）。
    pub const fn memory() -> Self {
        CalEngineInfo {
            engine: "memory-calibration",
            synthetic: true,
            backend: "in-memory-test-double",
            real_audio: false,
            note: "in-memory calibration test double; not a daemon job",
        }
    }

    /// 是否可把回执标成 actual（只有真设备测量 + 真实 all-ack 才允许）。
    pub const fn is_actual(&self) -> bool {
        !self.synthetic
    }

    /// JSON 投影（REST 响应/页面共用）。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("engine", JsonValue::str(self.engine)),
            ("synthetic", JsonValue::Bool(self.synthetic)),
            ("backend", JsonValue::str(self.backend)),
            ("real_audio", JsonValue::Bool(self.real_audio)),
            ("note", JsonValue::str(self.note)),
        ])
    }
}

/// 校准 job 后端接口（T60 JobManager 的 REST 投影；E-3）。
pub trait CalibrationPort: Send {
    /// 启动 job（同一时刻只允许一个 active；冲突返回 Rejected(ERR_SWITCH_BUSY)）。
    fn start(&mut self, request: &CalStartRequest) -> Result<CalJobView, CalPortError>;
    /// 读 job（&mut：真实 port 需要从 job/状态机刷新进度投影）。
    fn get(&mut self, job_id: &str) -> Option<CalJobView>;
    /// 列 job（按 id 升序；&mut 同 get 的刷新理由）。
    fn list(&mut self) -> Vec<CalJobView>;
    /// 取消（协作式；终态 job 取消 -> Rejected(ERR_SWITCH_BUSY)）。
    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError>;
    /// 应用 prepared job（必须返回 applied_version 实际 ack）。
    fn apply(&mut self, job_id: &str) -> Result<CalJobView, CalPortError>;
    /// 复测（对新 job；返回新 job view）。
    fn retest(&mut self, job_id: &str) -> Result<CalJobView, CalPortError>;
    /// 回滚到 last-good（返回 rolled_back job view，必须带 last_good_version）。
    fn rollback(&mut self, job_id: &str) -> Result<CalJobView, CalPortError>;

    /// port 身份/接线事实（F-13 同口径：synthetic 回执必须显式标注）。
    ///
    /// 默认 = unknown/synthetic（保守方向：忘记实现的 port 也不会被当成 actual）。
    fn engine_info(&self) -> CalEngineInfo {
        CalEngineInfo::unknown()
    }

    /// level/delay/bass 每 lane 结果（host 字段未落地时默认 None；REST 显式标 pending）。
    fn lane_results(&mut self, _job_id: &str) -> Option<calibration_contract::CalLaneResults> {
        None
    }

    /// 应用 level trims / delays / bass（host 字段未落地时 501，不伪造 applied）。
    fn apply_trims(
        &mut self,
        _request: &calibration_contract::CalApplyRequest,
    ) -> Result<calibration_contract::CalAppliedState, CalPortError> {
        Err(CalPortError::PendingHostField(
            "level/delay/bass apply is pending host fields (host has no trim/delay/bass apply endpoint yet)"
                .to_string(),
        ))
    }

    /// 清除已应用 level trims / delays / bass（host 字段未落地时 501）。
    fn clear_trims(
        &mut self,
        _targets: &[String],
    ) -> Result<calibration_contract::CalAppliedState, CalPortError> {
        Err(CalPortError::PendingHostField(
            "level/delay/bass clear is pending host fields (host has no trim/delay/bass clear endpoint yet)"
                .to_string(),
        ))
    }
}

/// engine 身份/接线事实（F-13：daemon 不得把内存替身回执当成 actual）。
///
/// 响应体必须原样带出 `engine`/`synthetic`；`synthetic=true` 表示回执来自内存模型
/// （没有真实 worker all-ack / 真实输出），页面不得把它显示成「已生效（actual）」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineInfo {
    /// engine 实现名（"memory" = 测试替身；"host" = rt-host PlanRegistry）。
    pub engine: &'static str,
    /// true = actual ack 来自内存模型（无真实 block/all-ack），禁止标成 actual。
    pub synthetic: bool,
    /// 模式切换是否走 T44 ModeController 真状态机（epoch/servo/回滚语义）。
    pub mode_controller: bool,
    /// 模式服务后端（"systemd" = 板端接线；"simulated" = 内存模拟；"not_wired"）。
    pub mode_services: &'static str,
}

impl EngineInfo {
    /// 离线内存 engine（测试替身/开发回环）：回执全部 synthetic。
    pub const fn memory() -> Self {
        EngineInfo {
            engine: "memory",
            synthetic: true,
            mode_controller: false,
            mode_services: "not_wired",
        }
    }

    /// 是否可把该回执标成 actual（只有非 synthetic 的真实 host engine 可以）。
    pub const fn is_actual(&self) -> bool {
        !self.synthetic
    }
}

/// 模式切换 actual 证据（T44 ModeController 产出；F-28：成功切换必须有新 epoch/唯一 servo）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeStateEvidence {
    /// 本次切换后的 epoch（0 = 尚未建立；成功切换必须 > 0）。
    pub epoch: u64,
    /// 切换序号（每次切换尝试 +1）。
    pub switch_seq: u64,
    /// 唯一 servo owner（播放模式成功切换必须 Some）。
    pub servo_owner: Option<String>,
    /// servo claim 序号（进程内单调）。
    pub servo_claims: u64,
    /// 最近一次切换是否已回滚（失败路径）。
    pub rolled_back: bool,
    /// A03 退出码（成功 0 / 失败回滚 9）。
    pub last_exit_code: u8,
    /// 回退动作（"none"/"off"/"last_good"）。
    pub fallback: &'static str,
}

/// 核心运行时 engine 接口（T26 用 rt-host swap::PlanRegistry 实现；T42 只消费）。
///
/// 不变量（core-v1 §5.3 / T20）：
/// - prepare 只做非 RT 校验/admission，不改输出；
/// - commit 必须在 plan_version 已出现在至少 1 个完整 block 且 4 worker all-ack 后
///   才返回 Ok（真实实现负责；REST 只信 engine 返回，不自行提前 ack）；
/// - 失败必须返回 RtStatus（117/118/119/120/121/122/123 等），不得假成功。
pub trait RuntimeEngine: Send {
    /// 当前 actual 快照。
    fn snapshot(&self) -> EngineSnapshot;
    /// coeff 版本是否已登记/可用（PUT coeff 前置校验；X18）。
    fn coeff_known(&self, coeff: &CoeffRef) -> bool;
    /// prepare（非 RT admission；不改输出）。
    fn prepare(&mut self, desired: &RuntimeConfig) -> Result<PrepareOutcome, RtStatus>;
    /// commit（block 边界 + all-ack 后返回 actual ack）。
    fn commit(&mut self, engine_token: u64) -> Result<CommitOutcome, RtStatus>;
    /// abort staged plan（PlanAborted；未生效无输出承诺）。
    fn abort(&mut self, engine_token: u64) -> RtStatus;
    /// 写 volume（实际生效后返回 actual；mute 独立）。
    fn set_volume(&mut self, volume_db: f64) -> Result<VolumeAck, RtStatus>;
    /// 写 mute（幂等；不改 volume）。
    fn set_mute(&mut self, mute: bool) -> Result<MuteAck, RtStatus>;
    /// 模式切换（T44 实现真实状态机；REST 返回 202 + switch_state）。
    fn switch_mode(&mut self, mode: ControlMode) -> Result<ModeAck, RtStatus>;

    /// engine 身份/接线事实（F-13：synthetic 回执必须显式标注）。
    ///
    /// 无默认实现：任何新 engine 都必须明确声明自己是不是真实 host engine，
    /// 不允许「默认看起来像 actual」。
    fn engine_info(&self) -> EngineInfo;

    /// 模式切换 actual 证据（F-28：T44 ModeController 接线后返回 epoch/servo/回滚事实）。
    ///
    /// None = 模式状态机未接线；REST 必须把该回执标成 synthetic（不得当 actual）。
    fn mode_evidence(&self) -> Option<ModeStateEvidence> {
        None
    }
}

/// 离线内存 engine（单元测试/开发回环用；不触碰设备/不启线程）。
///
/// 这是**测试替身**，不是生产 engine；生产 wiring 归 T26（PlanRegistry + 真实 ALSA/
/// worker ack）。它仍严格按 prepare/commit 两阶段与 actual ack 语义实现，用于验证 REST
/// 语义（不假 ack 的消费者侧）。
///
/// F-13：本 engine 的 `commit` 在同一调用里写 `blocks_committed=1`/`ack_mask=0xF`，
/// 这些是**合成的**（没有真实 block/worker）。`engine_info()` 恒返回
/// `synthetic=true`，REST 响应必须原样带出，页面不得显示成 actual。真实 ack 只能由
/// T26 的 rt-host engine 提供。
#[derive(Debug)]
pub struct MemoryEngine {
    snapshot: EngineSnapshot,
    prepared: BTreeMap<u64, (RuntimeConfig, ws::PreparedConfig)>,
    next_token: u64,
    next_plan_version: u32,
    current_plan_version: u32,
    next_version: u64,
    switch_busy: bool,
    fail_prepare: Option<RtStatus>,
    fail_commit: Option<RtStatus>,
    fail_volume: Option<RtStatus>,
    fail_mode: Option<RtStatus>,
    bad_commit_ack: bool,
    prepare_calls: u64,
    commit_calls: u64,
    abort_calls: u64,
}

impl MemoryEngine {
    /// 默认 engine（4.1.2/toslink/48k/128/-12dB/未 mute/1 epoch）。
    pub fn new() -> Self {
        let target = TargetSpec::p1_412();
        let baseline = CoeffRef::new("cal-baseline", hex_lower(&sha256(b"cal-baseline")))
            .expect("baseline coeff");
        MemoryEngine {
            snapshot: EngineSnapshot {
                runtime_state: "Running".to_string(),
                device_owner: "playback".to_string(),
                epoch: 1,
                actual_version: "v0001".to_string(),
                graph_id: hex_lower(&sha256(b"p1-412-baseline")),
                coeff: None,
                last_good_coeff: Some(baseline.clone()),
                available_coeffs: vec![baseline],
                target_hash: target.fingerprint(),
                target,
                mode: ControlMode::Toslink,
                mode_profile: Some("toslink-2.0".to_string()),
                sample_rate: DEFAULT_SAMPLE_RATE,
                chunksize: DEFAULT_CHUNKSIZE,
                volume_db: -12.0,
                mute: false,
                xruns: 0,
                deadline_misses: 0,
                clipped_samples: 0,
                calibration_owner: false,
                committed_unix: now_unix(),
            },
            prepared: BTreeMap::new(),
            next_token: 1,
            next_plan_version: 2,
            current_plan_version: 1,
            next_version: 1,
            switch_busy: false,
            fail_prepare: None,
            fail_commit: None,
            fail_volume: None,
            fail_mode: None,
            bad_commit_ack: false,
            prepare_calls: 0,
            commit_calls: 0,
            abort_calls: 0,
        }
    }

    /// 追加一个可用 coeff 版本。
    pub fn add_coeff(mut self, coeff: CoeffRef) -> Self {
        self.snapshot.available_coeffs.push(coeff);
        self
    }

    /// 设置 last-good coeff。
    pub fn with_last_good(mut self, coeff: CoeffRef) -> Self {
        self.snapshot.last_good_coeff = Some(coeff);
        self
    }

    /// 设置切换忙（409 switch_busy 测试）。
    pub fn set_switch_busy(&mut self, busy: bool) {
        self.switch_busy = busy;
    }

    /// 设置设备 owner（idle/playback/calibration）。
    pub fn set_device_owner(&mut self, owner: &str) {
        self.snapshot.device_owner = owner.to_string();
        self.snapshot.calibration_owner = owner == "calibration";
    }

    /// 注入 prepare 错误（RtStatus 映射测试）。
    pub fn set_fail_prepare(&mut self, status: Option<RtStatus>) {
        self.fail_prepare = status;
    }

    /// 注入 commit 错误。
    pub fn set_fail_commit(&mut self, status: Option<RtStatus>) {
        self.fail_commit = status;
    }

    /// 注入 volume 错误。
    pub fn set_fail_volume(&mut self, status: Option<RtStatus>) {
        self.fail_volume = status;
    }

    /// 注入 mode 错误。
    pub fn set_fail_mode(&mut self, status: Option<RtStatus>) {
        self.fail_mode = status;
    }

    /// 让 commit 返回非法 ack（REST 必须 500 不假成功）。
    pub fn set_bad_commit_ack(&mut self, bad: bool) {
        self.bad_commit_ack = bad;
    }

    /// prepare 调用次数（测试断言）。
    pub fn prepare_calls(&self) -> u64 {
        self.prepare_calls
    }

    /// commit 调用次数（测试断言）。
    pub fn commit_calls(&self) -> u64 {
        self.commit_calls
    }

    /// abort 调用次数。
    pub fn abort_calls(&self) -> u64 {
        self.abort_calls
    }

    fn graph_id_for(config: &RuntimeConfig) -> String {
        hex_lower(&sha256(
            format!("graph:{}", config.fingerprint()).as_bytes(),
        ))
    }
}

impl Default for MemoryEngine {
    fn default() -> Self {
        MemoryEngine::new()
    }
}

impl RuntimeEngine for MemoryEngine {
    fn engine_info(&self) -> EngineInfo {
        EngineInfo::memory()
    }

    fn snapshot(&self) -> EngineSnapshot {
        self.snapshot.clone()
    }

    fn coeff_known(&self, coeff: &CoeffRef) -> bool {
        self.snapshot
            .available_coeffs
            .iter()
            .any(|candidate| candidate.version == coeff.version && candidate.sha256 == coeff.sha256)
    }

    fn prepare(&mut self, desired: &RuntimeConfig) -> Result<PrepareOutcome, RtStatus> {
        self.prepare_calls += 1;
        if let Some(status) = self.fail_prepare {
            return Err(status);
        }
        if self.snapshot.device_owner == "calibration" {
            return Err(RtStatus::ERR_DEVICE_BUSY);
        }
        if self.switch_busy {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if let Some(coeff) = &desired.coeff {
            if !self.coeff_known(coeff) {
                return Err(RtStatus::ERR_PLAN_STALE);
            }
        }
        let token = self.next_token;
        self.next_token += 1;
        let plan_version = self.next_plan_version;
        self.next_plan_version += 1;
        let admission = format!(
            "admitted:{}ch@{}Hz/B{}",
            desired.target.channels, desired.sample_rate, desired.chunksize
        );
        // T24 同口径 prepare 凭据（REST/WS 共用 ws::validate_commit_ack）。
        let prepared = ws::PreparedConfig {
            plan_version,
            graph_id_hex: MemoryEngine::graph_id_for(desired),
            config_sha256_hex: desired.fingerprint(),
            expected_workers: 4,
            admitted_bytes: 64 * 1024,
            admission_note: admission.clone(),
        };
        self.prepared
            .insert(token, (desired.clone(), prepared.clone()));
        Ok(PrepareOutcome {
            engine_token: token,
            planned_version: format!("pv-{plan_version}"),
            planned_target_hash: desired.target.fingerprint(),
            planned_coeff: desired.coeff.clone(),
            admission,
            prepared,
        })
    }

    fn commit(&mut self, engine_token: u64) -> Result<CommitOutcome, RtStatus> {
        self.commit_calls += 1;
        if let Some(status) = self.fail_commit {
            return Err(status);
        }
        if self.switch_busy {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        let (config, prepared) = self
            .prepared
            .remove(&engine_token)
            .ok_or(RtStatus::ERR_PLAN_STALE)?;
        if let Some(coeff) = &config.coeff {
            if !self.coeff_known(coeff) {
                return Err(RtStatus::ERR_PLAN_STALE);
            }
        }
        self.next_version += 1;
        let target_hash = config.target.fingerprint();
        let previous_coeff = self.snapshot.coeff.clone();
        self.snapshot.actual_version = format!("v{:04}", self.next_version);
        self.snapshot.graph_id = prepared.graph_id_hex.clone();
        self.snapshot.coeff = config.coeff.clone();
        if previous_coeff.is_some() {
            self.snapshot.last_good_coeff = previous_coeff;
        }
        self.snapshot.target_hash = target_hash.clone();
        self.snapshot.target = config.target.clone();
        self.snapshot.mode = config.mode;
        self.snapshot.mode_profile = Some(config.target.profile.clone());
        self.snapshot.sample_rate = config.sample_rate;
        self.snapshot.chunksize = config.chunksize;
        self.snapshot.epoch += 1;
        self.snapshot.committed_unix = now_unix();
        // T24 同口径 commit ack 证据：正常路径 = 1 个完整 block + 4 worker all-ack 0xF。
        // F-13：这里是内存模型**合成**的 ack（没有真实 block/worker），由 engine_info()
        // 的 synthetic=true 在 REST 响应里显式标注；bad_commit_ack 注入 =
        // blocks_committed=0，REST 侧 ws::validate_commit_ack 必须拒绝。
        let (blocks_committed, ack_mask) = if self.bad_commit_ack {
            (0u64, 0u32)
        } else {
            (1u64, 0xF)
        };
        let ack = ws::CommitAck {
            plan_version: prepared.plan_version,
            previous_plan_version: self.current_plan_version,
            graph_id_hex: prepared.graph_id_hex.clone(),
            config_sha256_hex: prepared.config_sha256_hex.clone(),
            expected_workers: prepared.expected_workers,
            blocks_committed,
            ack_mask,
            switched_at_block: 0,
            rolled_back: false,
        };
        self.current_plan_version = prepared.plan_version;
        Ok(CommitOutcome {
            actual_version: self.snapshot.actual_version.clone(),
            coeff: config.coeff,
            target_hash,
            epoch: self.snapshot.epoch,
            committed_unix: self.snapshot.committed_unix,
            ack,
        })
    }

    fn abort(&mut self, engine_token: u64) -> RtStatus {
        self.abort_calls += 1;
        if self.prepared.remove(&engine_token).is_some() {
            RtStatus::OK
        } else {
            RtStatus::ERR_PLAN_STALE
        }
    }

    fn set_volume(&mut self, volume_db: f64) -> Result<VolumeAck, RtStatus> {
        if let Some(status) = self.fail_volume {
            return Err(status);
        }
        if self.snapshot.device_owner == "calibration" {
            return Err(RtStatus::ERR_DEVICE_BUSY);
        }
        self.snapshot.volume_db = volume_db;
        Ok(VolumeAck {
            volume_db: self.snapshot.volume_db,
            mute: self.snapshot.mute,
        })
    }

    fn set_mute(&mut self, mute: bool) -> Result<MuteAck, RtStatus> {
        if let Some(status) = self.fail_volume {
            return Err(status);
        }
        if self.snapshot.device_owner == "calibration" {
            return Err(RtStatus::ERR_DEVICE_BUSY);
        }
        self.snapshot.mute = mute;
        Ok(MuteAck {
            mute: self.snapshot.mute,
            volume_db: self.snapshot.volume_db,
        })
    }

    fn switch_mode(&mut self, mode: ControlMode) -> Result<ModeAck, RtStatus> {
        if let Some(status) = self.fail_mode {
            return Err(status);
        }
        if self.switch_busy {
            return Err(RtStatus::ERR_SWITCH_BUSY);
        }
        if self.snapshot.device_owner == "calibration" {
            return Err(RtStatus::ERR_DEVICE_BUSY);
        }
        self.snapshot.mode = mode;
        let profile = match mode {
            ControlMode::Off => None,
            ControlMode::Toslink => Some("toslink-2.0".to_string()),
            ControlMode::TargetMultichannel => Some(self.snapshot.target.profile.clone()),
            ControlMode::Aes67 => Some("aes67-48k".to_string()),
            ControlMode::Dante => Some("dante-96k".to_string()),
        };
        self.snapshot.mode_profile = profile.clone();
        Ok(ModeAck {
            mode,
            profile,
            target_hash: self.snapshot.target_hash.clone(),
            sample_rate: self.snapshot.sample_rate,
            chunksize: self.snapshot.chunksize,
            switch_state: "switched".to_string(),
        })
    }
}

/// 离线内存校准 port（单元测试/开发回环用；不是 T60 真实状态机）。
#[derive(Debug)]
pub struct MemoryCalibration {
    jobs: BTreeMap<String, CalJobView>,
    order: Vec<String>,
    next_id: u64,
    active: Option<String>,
    fail_next: Option<RtStatus>,
    busy: bool,
}

impl MemoryCalibration {
    /// 默认（一个 last-good 基线版本）。
    pub fn new() -> Self {
        MemoryCalibration {
            jobs: BTreeMap::new(),
            order: Vec::new(),
            next_id: 1,
            active: None,
            fail_next: None,
            busy: false,
        }
    }

    /// 注入下一条错误。
    pub fn set_fail_next(&mut self, status: Option<RtStatus>) {
        self.fail_next = status;
    }

    /// 设置忙（409 测试）。
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    fn take_fail(&mut self) -> Option<RtStatus> {
        self.fail_next.take()
    }

    fn create_job(&mut self, kind: &str) -> CalJobView {
        let id = format!("job-{:04}", self.next_id);
        self.next_id += 1;
        let now = now_unix();
        let view = CalJobView {
            job_id: id.clone(),
            kind: kind.to_string(),
            state: "queued".to_string(),
            stage: None,
            progress: 0.0,
            applied_version: None,
            last_good_version: Some("cal-baseline".to_string()),
            failure: None,
            created_unix: now,
            updated_unix: now,
        };
        self.jobs.insert(id.clone(), view.clone());
        self.order.push(id.clone());
        self.active = Some(id);
        view
    }

    fn touch(
        &mut self,
        job_id: &str,
        mutate: impl FnOnce(&mut CalJobView),
    ) -> Result<CalJobView, CalPortError> {
        let view = self.jobs.get_mut(job_id).ok_or(CalPortError::NotFound)?;
        mutate(view);
        view.updated_unix = now_unix();
        let out = view.clone();
        Ok(out)
    }
}

impl Default for MemoryCalibration {
    fn default() -> Self {
        MemoryCalibration::new()
    }
}

impl CalibrationPort for MemoryCalibration {
    fn start(&mut self, request: &CalStartRequest) -> Result<CalJobView, CalPortError> {
        if let Some(status) = self.take_fail() {
            return Err(CalPortError::Rejected(status));
        }
        if self.busy {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        if let Some(active) = &self.active {
            if let Some(job) = self.jobs.get(active) {
                if !job.is_terminal() {
                    return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                }
            }
        }
        Ok(self.create_job(&request.kind))
    }

    fn get(&mut self, job_id: &str) -> Option<CalJobView> {
        self.jobs.get(job_id).cloned()
    }

    fn list(&mut self) -> Vec<CalJobView> {
        self.order
            .iter()
            .filter_map(|id| self.jobs.get(id).cloned())
            .collect()
    }

    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if let Some(status) = self.take_fail() {
            return Err(CalPortError::Rejected(status));
        }
        let job = self.jobs.get(job_id).ok_or(CalPortError::NotFound)?;
        if job.is_terminal() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        let view = self.touch(job_id, |job| {
            job.state = "cancelled".to_string();
            job.stage = None;
            job.progress = 1.0;
            job.failure = Some("cancelled by user".to_string());
        })?;
        self.active = None;
        Ok(view)
    }

    fn apply(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if let Some(status) = self.take_fail() {
            return Err(CalPortError::Rejected(status));
        }
        let job = self.jobs.get(job_id).ok_or(CalPortError::NotFound)?;
        if job.state != "prepared" {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        let view = self.touch(job_id, |job| {
            job.state = "complete".to_string();
            job.stage = None;
            job.progress = 1.0;
            job.applied_version = Some(format!("{}-applied", job.job_id));
            job.last_good_version = Some("cal-baseline".to_string());
        })?;
        self.active = None;
        Ok(view)
    }

    fn retest(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if let Some(status) = self.take_fail() {
            return Err(CalPortError::Rejected(status));
        }
        let job = self.jobs.get(job_id).ok_or(CalPortError::NotFound)?;
        if job.applied_version.is_none() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        Ok(self.create_job("retest"))
    }

    fn rollback(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if let Some(status) = self.take_fail() {
            return Err(CalPortError::Rejected(status));
        }
        let job = self.jobs.get(job_id).ok_or(CalPortError::NotFound)?;
        if job.last_good_version.is_none() {
            return Err(CalPortError::Rejected(RtStatus::ERR_PLAN_STALE));
        }
        let view = self.touch(job_id, |job| {
            job.state = "rolled_back".to_string();
            job.stage = None;
            job.progress = 1.0;
            job.failure = None;
        })?;
        self.active = None;
        Ok(view)
    }

    fn engine_info(&self) -> CalEngineInfo {
        CalEngineInfo::memory()
    }
}

// ---------------------------------------------------------------------------
// 鉴权（R02-R06/K03/X21）与配置
// ---------------------------------------------------------------------------

/// 密码记录（PBKDF2-HMAC-SHA256；E-2：非 Argon2id，见文件头偏差）。
#[derive(Debug, Clone)]
struct PasswordRecord {
    salt: [u8; 16],
    iterations: u32,
    hash: [u8; 32],
}

impl PasswordRecord {
    fn derive(password: &str, salt: [u8; 16], iterations: u32) -> Self {
        let digest = pbkdf2_sha256(password.as_bytes(), &salt, iterations, 32);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&digest);
        PasswordRecord {
            salt,
            iterations,
            hash,
        }
    }

    fn verify(&self, password: &str) -> bool {
        let digest = pbkdf2_sha256(password.as_bytes(), &self.salt, self.iterations, 32);
        constant_time_eq(&digest, &self.hash)
    }
}

/// 会话（Cookie + CSRF；K03）。
#[derive(Debug, Clone)]
struct Session {
    csrf: String,
    created_unix: u64,
    expires_unix: u64,
}

/// 鉴权状态（内存；持久化/多进程在 T70/daemon 层）。
#[derive(Debug)]
struct AuthState {
    password: Option<PasswordRecord>,
    token: Option<String>,
    failed_logins: u32,
    lockout_until: u64,
    sessions: BTreeMap<String, Session>,
    session_order: VecDeque<String>,
}

impl AuthState {
    fn new(config: &RestConfig) -> Self {
        AuthState {
            password: None,
            token: config.api_token.clone(),
            failed_logins: 0,
            lockout_until: 0,
            sessions: BTreeMap::new(),
            session_order: VecDeque::new(),
        }
    }
}

/// 鉴权方式（header token 免 CSRF；Cookie session 必须 CSRF）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// 直接 header token（X-TSPI-Token / Authorization: Bearer）。
    HeaderToken,
    /// Cookie 会话。
    Session,
}

impl AuthKind {
    /// REST auth_kind 名（R05）。
    pub const fn as_str(self) -> &'static str {
        match self {
            AuthKind::HeaderToken => "token",
            AuthKind::Session => "session",
        }
    }
}

/// 已认证上下文。
#[derive(Debug, Clone)]
struct AuthContext {
    kind: AuthKind,
    session_id: Option<String>,
    csrf: Option<String>,
}

/// REST 配置（每 service 一份；T70 可注入 token/TTL/限速参数）。
#[derive(Debug, Clone)]
pub struct RestConfig {
    /// healthz mock 字段（生产必须 false；R01）。
    pub mock: bool,
    /// 直接 API token（R04 token fallback；None = 未配置）。
    pub api_token: Option<String>,
    /// 密码最小长度（R03）。
    pub min_password_len: usize,
    /// 推荐密码长度（R02）。
    pub recommended_password_len: usize,
    /// 会话 TTL（秒）。
    pub session_ttl_secs: u64,
    /// 并发会话上限。
    pub max_sessions: usize,
    /// 请求体上限（X22）。
    pub max_body_bytes: usize,
    /// 连续失败上限（R04）。
    pub max_failed_logins: u32,
    /// 锁定时长（秒）。
    pub lockout_secs: u64,
    /// PBKDF2 迭代数（E-2；测试可用低值）。
    pub pbkdf2_iterations: u32,
    /// 是否允许 mode 切换（R11 未启用 403 测试）。
    pub enable_mode_switch: bool,
}

impl Default for RestConfig {
    fn default() -> Self {
        RestConfig {
            mock: false,
            api_token: None,
            min_password_len: MIN_PASSWORD_LEN,
            recommended_password_len: RECOMMENDED_PASSWORD_LEN,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            max_sessions: MAX_SESSIONS,
            max_body_bytes: MAX_BODY_BYTES,
            max_failed_logins: MAX_FAILED_LOGINS,
            lockout_secs: DEFAULT_LOCKOUT_SECS,
            pbkdf2_iterations: DEFAULT_PBKDF2_ITERATIONS,
            enable_mode_switch: true,
        }
    }
}

impl RestConfig {
    /// 测试配置（固定 token + 低 PBKDF2 迭代；不用于生产）。
    pub fn for_tests() -> Self {
        RestConfig {
            api_token: Some("test-token-0123456789abcdef".to_string()),
            pbkdf2_iterations: 1_000,
            ..RestConfig::default()
        }
    }
}

// ---------------------------------------------------------------------------
// 严格字段白名单（X02/X03）
// ---------------------------------------------------------------------------

fn invalid_json(message: impl Into<String>) -> RestError {
    RestError::bad_request(CODE_INVALID_JSON, message)
}

fn object_entries(
    value: &JsonValue,
    path: &'static str,
) -> Result<Vec<(String, JsonValue)>, RestError> {
    match value {
        JsonValue::Obj(entries) => Ok(entries.clone()),
        _ => Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            format!("{path} must be a JSON object"),
        )),
    }
}

fn parse_json_object_body(
    request: &RestRequest,
    path: &'static str,
    allow_empty: bool,
) -> Result<Vec<(String, JsonValue)>, RestError> {
    let text = std::str::from_utf8(&request.body)
        .map_err(|_| invalid_json("request body is not valid UTF-8"))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        if allow_empty {
            return Ok(Vec::new());
        }
        return Err(invalid_json(format!("{path} requires a JSON object body")));
    }
    let value = JsonValue::parse(trimmed).map_err(|error| {
        let reason = match error.kind {
            JsonParseErrorKind::DuplicateKey => "duplicate object key",
            JsonParseErrorKind::NonFiniteNumber => "non-finite number",
            JsonParseErrorKind::TooDeep => "nesting too deep",
            JsonParseErrorKind::Syntax => "syntax error",
        };
        invalid_json(format!(
            "invalid JSON body ({reason} at offset {})",
            error.offset
        ))
    })?;
    match value {
        JsonValue::Obj(entries) => Ok(entries),
        _ => Err(invalid_json(format!("{path} body must be a JSON object"))),
    }
}

/// 严格字段访问器（未知字段 -> 400 unknown_field；缺失 -> 400 missing_field；
/// 类型/取值 -> 400 invalid_field）。
struct Fields {
    path: &'static str,
    entries: Vec<(String, JsonValue)>,
}

impl Fields {
    fn new(entries: Vec<(String, JsonValue)>, path: &'static str) -> Self {
        Fields { path, entries }
    }

    fn take(&mut self, key: &str) -> Option<JsonValue> {
        let index = self.entries.iter().position(|(name, _)| name == key)?;
        Some(self.entries.remove(index).1)
    }

    fn take_opt(&mut self, key: &str) -> Option<JsonValue> {
        self.take(key)
    }

    fn take_required(&mut self, key: &str) -> Result<JsonValue, RestError> {
        self.take(key).ok_or_else(|| {
            RestError::bad_request(
                CODE_MISSING_FIELD,
                format!("missing required field '{key}' in {}", self.path),
            )
        })
    }

    fn take_str(&mut self, key: &str) -> Result<String, RestError> {
        let value = self.take_required(key)?;
        match value {
            JsonValue::Str(text) => Ok(text),
            _ => Err(self.type_error(key, "a string")),
        }
    }

    fn take_opt_str(&mut self, key: &str) -> Result<Option<String>, RestError> {
        match self.take(key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Str(text)) => Ok(Some(text)),
            Some(_) => Err(self.type_error(key, "a string or null")),
        }
    }

    fn take_bool(&mut self, key: &str) -> Result<bool, RestError> {
        let value = self.take_required(key)?;
        match value {
            JsonValue::Bool(flag) => Ok(flag),
            _ => Err(self.type_error(key, "a boolean")),
        }
    }

    fn take_opt_bool(&mut self, key: &str) -> Result<Option<bool>, RestError> {
        match self.take(key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Bool(flag)) => Ok(Some(flag)),
            Some(_) => Err(self.type_error(key, "a boolean or null")),
        }
    }

    fn take_opt_f64(&mut self, key: &str) -> Result<Option<f64>, RestError> {
        match self.take(key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Num(number)) if number.is_finite() => Ok(Some(number)),
            Some(_) => Err(self.type_error(key, "a finite number or null")),
        }
    }

    fn take_opt_u32(&mut self, key: &str) -> Result<Option<u32>, RestError> {
        match self.take(key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Num(number)) => {
                if number.fract() == 0.0 && number >= 0.0 && number <= f64::from(u32::MAX) {
                    Ok(Some(number as u32))
                } else {
                    Err(self.type_error(key, "an unsigned 32-bit integer"))
                }
            }
            Some(_) => Err(self.type_error(key, "an unsigned 32-bit integer")),
        }
    }

    fn take_u16(&mut self, key: &str) -> Result<u16, RestError> {
        let value = self.take_required(key)?;
        match value {
            JsonValue::Num(number)
                if number.fract() == 0.0 && number >= 0.0 && number <= f64::from(u16::MAX) =>
            {
                Ok(number as u16)
            }
            _ => Err(self.type_error(key, "an unsigned 16-bit integer")),
        }
    }

    fn take_opt_u16(&mut self, key: &str) -> Result<Option<u16>, RestError> {
        match self.take(key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Num(number)) => {
                if number.fract() == 0.0 && number >= 0.0 && number <= f64::from(u16::MAX) {
                    Ok(Some(number as u16))
                } else {
                    Err(self.type_error(key, "an unsigned 16-bit integer"))
                }
            }
            Some(_) => Err(self.type_error(key, "an unsigned 16-bit integer")),
        }
    }

    fn type_error(&self, key: &str, expected: &str) -> RestError {
        RestError::bad_request(
            CODE_INVALID_FIELD,
            format!("field '{key}' in {} must be {expected}", self.path),
        )
    }

    fn finish(self) -> Result<(), RestError> {
        if let Some((key, _)) = self.entries.into_iter().next() {
            return Err(RestError::bad_request(
                CODE_UNKNOWN_FIELD,
                format!("unknown field '{key}' in {}", self.path),
            ));
        }
        Ok(())
    }
}

fn parse_role_list(value: &JsonValue, path: &'static str) -> Result<Vec<RoleDac>, RestError> {
    let items = match value {
        JsonValue::Arr(items) => items,
        _ => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("{path} must be an array"),
            ))
        }
    };
    let mut roles = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let label = format!("{path}[{index}]");
        let entries = object_entries(item, "target.roles")?;
        let mut fields = Fields::new(entries, "target.roles");
        let role = fields.take_str("role")?;
        let dac = fields.take_u16("dac")?;
        fields.finish()?;
        if dac > u16::from(u8::MAX) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("{label}.dac must fit in u8"),
            ));
        }
        roles.push(RoleDac {
            role,
            dac: dac as u8,
        });
    }
    Ok(roles)
}

fn parse_u8_list(value: &JsonValue, path: &'static str) -> Result<Vec<u8>, RestError> {
    let items = match value {
        JsonValue::Arr(items) => items,
        _ => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("{path} must be an array"),
            ))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            JsonValue::Num(number)
                if number.fract() == 0.0 && *number >= 0.0 && *number <= f64::from(u8::MAX) =>
            {
                out.push(*number as u8);
            }
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    format!("{path} entries must be integers in 0..=255"),
                ))
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Service 内部状态
// ---------------------------------------------------------------------------

/// 成功的 prepare 记录（未生效；commit 前后判定 stale）。
#[derive(Debug, Clone)]
struct PreparedPlan {
    prepare_id: String,
    engine_token: u64,
    fingerprint: String,
    planned_version: String,
    planned_target_hash: String,
    planned_coeff: Option<CoeffRef>,
    admission: String,
    created_unix: u64,
    invalidated: bool,
    /// T24 同口径 prepare 凭据（commit 时用 ws::validate_commit_ack 核对）。
    prepared: ws::PreparedConfig,
}

/// commit 成功后的幂等重放记录。
#[derive(Debug, Clone)]
struct CommitAck {
    actual_version: String,
    coeff: Option<CoeffRef>,
    target_hash: String,
    epoch: u64,
    committed_unix: u64,
    /// T24 同口径 commit ack 证据（幂等重放原样返回）。
    ack: ws::CommitAck,
}

struct Inner {
    config: RestConfig,
    engine: Box<dyn RuntimeEngine + Send>,
    calibration: Option<Box<dyn CalibrationPort + Send>>,
    desired: RuntimeConfig,
    prepared: Option<PreparedPlan>,
    invalidated: BTreeSet<String>,
    committed: BTreeMap<String, CommitAck>,
    commit_order: VecDeque<String>,
    next_prepare: u64,
    auth: AuthState,
    retired: BTreeMap<String, String>,
    /// daemon 侧校准设置（mic/cal_file/bass；POST /api/calibration/settings 更新）。
    cal_settings: calibration_contract::CalibrationSettings,
    /// 最近一次 apply/clear 回执状态（daemon 内存；不标 applied_to_audio）。
    cal_applied: calibration_contract::CalAppliedState,
}

/// 自有 REST 控制面（transport-agnostic；T26/T70 提供 HTTP 接线）。
pub struct RestService {
    inner: Mutex<Inner>,
    started: Instant,
    started_unix: u64,
}

impl RestService {
    /// 默认配置 + 指定 engine。
    pub fn new(engine: Box<dyn RuntimeEngine + Send>) -> Self {
        RestService::with_config(engine, RestConfig::default())
    }

    /// 指定配置 + engine。
    pub fn with_config(engine: Box<dyn RuntimeEngine + Send>, config: RestConfig) -> Self {
        let auth = AuthState::new(&config);
        let inner = Inner {
            config,
            engine,
            calibration: None,
            desired: RuntimeConfig::default(),
            prepared: None,
            invalidated: BTreeSet::new(),
            committed: BTreeMap::new(),
            commit_order: VecDeque::new(),
            next_prepare: 0,
            auth,
            retired: BTreeMap::new(),
            cal_settings: calibration_contract::CalibrationSettings::default(),
            cal_applied: calibration_contract::CalAppliedState::default(),
        };
        RestService {
            inner: Mutex::new(inner),
            started: Instant::now(),
            started_unix: now_unix(),
        }
    }

    /// 挂接校准 port（T60 JobManager 适配；E-3）。
    pub fn with_calibration(self, port: Box<dyn CalibrationPort + Send>) -> Self {
        self.lock_inner().calibration = Some(port);
        self
    }

    /// 注册退役端点（X15；一般 410 retired + replacement）。
    pub fn register_retired(&self, path: &str, replacement: &str) {
        self.lock_inner()
            .retired
            .insert(path.to_string(), replacement.to_string());
    }

    /// builder 形式注册退役端点。
    pub fn with_retired(self, path: &str, replacement: &str) -> Self {
        self.register_retired(path, replacement);
        self
    }

    /// 处理一个请求（线程安全；内部控制面互斥）。
    pub fn handle(&self, request: &RestRequest) -> RestResponse {
        match self.dispatch(request) {
            Ok(response) => response,
            Err(error) => RestResponse::from_error(&error),
        }
    }

    /// 端点清单（K01 覆盖核对/T70 路由注册辅助；仅已实现端点）。
    pub fn routes() -> Vec<(&'static str, &'static str)> {
        vec![
            ("GET", "/healthz"),
            ("GET", "/api/auth/status"),
            ("POST", "/api/auth/setup"),
            ("POST", "/api/auth/login"),
            ("GET", "/api/auth/session"),
            ("POST", "/api/auth/logout"),
            ("GET", "/api/runtime/status"),
            ("GET", "/api/runtime/config"),
            ("PUT", "/api/runtime/config"),
            ("POST", "/api/runtime/prepare"),
            ("POST", "/api/runtime/commit"),
            ("POST", "/api/runtime/abort"),
            ("GET", "/api/runtime/volume"),
            ("PUT", "/api/runtime/volume"),
            ("GET", "/api/runtime/mute"),
            ("PUT", "/api/runtime/mute"),
            ("GET", "/api/runtime/mode"),
            ("PUT", "/api/runtime/mode"),
            ("GET", "/api/runtime/target"),
            ("PUT", "/api/runtime/target"),
            ("GET", "/api/runtime/coeff"),
            ("PUT", "/api/runtime/coeff"),
            ("GET", "/api/mode"),
            ("PUT", "/api/mode"),
            ("GET", "/api/mode/script-status"),
            ("GET", "/api/audio/format"),
            ("PUT", "/api/audio/format"),
            ("GET", "/api/channelmap/input-mode"),
            ("PUT", "/api/channelmap/input-mode"),
            ("GET", "/api/calibration/jobs"),
            ("POST", "/api/calibration/jobs"),
            ("GET", "/api/calibration/jobs/{id}"),
            ("POST", "/api/calibration/jobs/{id}/cancel"),
            ("POST", "/api/calibration/jobs/{id}/apply"),
            ("POST", "/api/calibration/jobs/{id}/retest"),
            ("POST", "/api/calibration/jobs/{id}/rollback"),
            ("GET", "/api/calibration/settings"),
            ("POST", "/api/calibration/settings"),
            ("POST", "/api/calibration/settings/cal-file"),
            ("GET", "/api/calibration/status"),
            ("POST", "/api/calibration/apply"),
            ("POST", "/api/calibration/clear"),
            ("POST", "/api/calibration/run"),
        ]
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn dispatch(&self, request: &RestRequest) -> Result<RestResponse, RestError> {
        check_api_version(request)?;
        let mut inner = self.lock_inner();
        if request.body.len() > inner.config.max_body_bytes {
            return Err(RestError::payload_too_large(inner.config.max_body_bytes));
        }
        let route = route_for(&inner, request)?;
        if !route.requires_auth() {
            return match route {
                Route::Healthz => Ok(self.handle_healthz(&inner)),
                Route::AuthStatus => Ok(handle_auth_status(&inner)),
                Route::AuthSetup => handle_auth_setup(&mut inner, request),
                Route::AuthLogin => handle_auth_login(&mut inner, request),
                _ => Err(RestError::internal("route/auth classification mismatch")),
            };
        }
        let context = authenticate(&mut inner, request, now_unix())?;
        if request.method.is_write() {
            require_csrf(&context, request)?;
        }
        match route {
            Route::AuthSession => Ok(handle_auth_session(&inner, &context)),
            Route::AuthLogout => handle_auth_logout(&mut inner, &context),
            _ => dispatch_protected(&mut inner, route, request),
        }
    }

    fn handle_healthz(&self, inner: &Inner) -> RestResponse {
        let uptime = self
            .started
            .elapsed()
            .as_secs()
            .max(now_unix().saturating_sub(self.started_unix));
        RestResponse::json(
            200,
            &JsonValue::obj(vec![
                ("status", JsonValue::str("ok")),
                ("version", JsonValue::str(REST_VERSION)),
                ("mock", JsonValue::Bool(inner.config.mock)),
                ("uptime_secs", JsonValue::from(uptime)),
                ("now_unix", JsonValue::from(now_unix())),
            ]),
        )
    }
}

// ===========================================================================
// PART 5/5 —— 路由 / 鉴权 / 受保护 handler / actual-ack 消费（WBS T42 收口）
// ===========================================================================
//
// 本段补齐 dispatch() 依赖的路由层、R02-R06 鉴权、R48/R49 handler 与 R47/X15
// 退役语义；所有 handler 只消费 RuntimeEngine/CalibrationPort 契约，不伪造 actual。

/// 校准 job 列表 schema（R48）。
pub const CAL_JOBS_SCHEMA: &str = "tspi-cal-jobs/v1";
/// 校准 job 单项 schema（R48）。
pub const CAL_JOB_SCHEMA: &str = "tspi-cal-job/v1";
/// 旧客户端 token 头（KEEP R04：S1 使用 x-auth-token；查找大小写不敏感）。
pub const LEGACY_TOKEN_HEADER: &str = "X-Auth-Token";
/// token 头兼容集合（先查新栈头，再查 S1 遗留头）。
const TOKEN_HEADERS: [&str; 2] = [TOKEN_HEADER, LEGACY_TOKEN_HEADER];

/// 已解析路由（route_for 的唯一输出；鉴权/handler 前已完成路径/方法校验）。
#[derive(Debug, Clone)]
enum Route {
    Healthz,
    AuthStatus,
    AuthSetup,
    AuthLogin,
    AuthSession,
    AuthLogout,
    RuntimeStatus,
    RuntimeConfigGet,
    RuntimeConfigPut,
    RuntimePrepare,
    RuntimeCommit,
    RuntimeAbort,
    RuntimeVolumeGet,
    RuntimeVolumePut,
    RuntimeMuteGet,
    RuntimeMutePut,
    RuntimeModeGet,
    RuntimeModePut,
    RuntimeTargetGet,
    RuntimeTargetPut,
    RuntimeCoeffGet,
    RuntimeCoeffPut,
    LegacyModeGet,
    LegacyModePut,
    LegacyModeScriptStatus,
    AudioFormatGet,
    AudioFormatPut,
    InputModeGet,
    InputModePut,
    CalJobsList,
    CalJobsStart,
    CalJobGet(String),
    CalJobCancel(String),
    CalJobApply(String),
    CalJobRetest(String),
    CalJobRollback(String),
    CalSettingsGet,
    CalSettingsPost,
    CalSettingsCalFilePost,
    CalStatusGet,
    CalApplyPost,
    CalClearPost,
    CalRunPost,
    Retired { replacement: String },
}

impl Route {
    /// 是否要求认证（R01/R02/R03/R04 匿名；其余全部受保护）。
    fn requires_auth(&self) -> bool {
        !matches!(
            self,
            Route::Healthz | Route::AuthStatus | Route::AuthSetup | Route::AuthLogin
        )
    }
}

/// 路径规范化：只去尾部斜杠（root 保留）；query/百分号解码由传输层完成。
fn normalize_path(path: &str) -> String {
    let trimmed = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 校准 job id 字符集（R48：路径段只允许安全字符；不合法按未知路径 404 处理）。
fn is_safe_cal_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_VERSION_CHARS
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 解析 /api/calibration/jobs/{id}[/{action}]。
fn cal_job_path(path: &str) -> Option<(String, Option<String>)> {
    let rest = path.strip_prefix("/api/calibration/jobs/")?;
    let mut segments = rest.split('/');
    let job_id = segments.next()?;
    if !is_safe_cal_job_id(job_id) {
        return None;
    }
    let action = segments.next().map(str::to_string);
    if segments.next().is_some() {
        return None;
    }
    if let Some(name) = &action {
        if !matches!(name.as_str(), "cancel" | "apply" | "retest" | "rollback") {
            return None;
        }
    }
    Some((job_id.to_string(), action))
}

/// X01/X15：路径 -> 路由（未知路径 404 JSON；已知路径错误方法 405 JSON）。
fn route_for(inner: &Inner, request: &RestRequest) -> Result<Route, RestError> {
    let path = normalize_path(&request.path);
    // X15：显式注册的退役端点优先（410 retired + replacement）。
    if let Some(replacement) = inner.retired.get(&path) {
        return Ok(Route::Retired {
            replacement: replacement.clone(),
        });
    }
    let method = request.method;
    let bad_method = || RestError::method_not_allowed(method.as_str());
    let route = match path.as_str() {
        "/healthz" => match method {
            Method::Get => Route::Healthz,
            _ => return Err(bad_method()),
        },
        "/api/auth/status" => match method {
            Method::Get => Route::AuthStatus,
            _ => return Err(bad_method()),
        },
        "/api/auth/setup" => match method {
            Method::Post => Route::AuthSetup,
            _ => return Err(bad_method()),
        },
        "/api/auth/login" => match method {
            Method::Post => Route::AuthLogin,
            _ => return Err(bad_method()),
        },
        "/api/auth/session" => match method {
            Method::Get => Route::AuthSession,
            _ => return Err(bad_method()),
        },
        "/api/auth/logout" => match method {
            Method::Post => Route::AuthLogout,
            _ => return Err(bad_method()),
        },
        "/api/runtime/status" => match method {
            Method::Get => Route::RuntimeStatus,
            _ => return Err(bad_method()),
        },
        "/api/runtime/config" => match method {
            Method::Get => Route::RuntimeConfigGet,
            Method::Put => Route::RuntimeConfigPut,
            _ => return Err(bad_method()),
        },
        "/api/runtime/prepare" => match method {
            Method::Post => Route::RuntimePrepare,
            _ => return Err(bad_method()),
        },
        "/api/runtime/commit" => match method {
            Method::Post => Route::RuntimeCommit,
            _ => return Err(bad_method()),
        },
        "/api/runtime/abort" => match method {
            Method::Post => Route::RuntimeAbort,
            _ => return Err(bad_method()),
        },
        "/api/runtime/volume" => match method {
            Method::Get => Route::RuntimeVolumeGet,
            Method::Put => Route::RuntimeVolumePut,
            _ => return Err(bad_method()),
        },
        "/api/runtime/mute" => match method {
            Method::Get => Route::RuntimeMuteGet,
            Method::Put => Route::RuntimeMutePut,
            _ => return Err(bad_method()),
        },
        "/api/runtime/mode" => match method {
            Method::Get => Route::RuntimeModeGet,
            Method::Put => Route::RuntimeModePut,
            _ => return Err(bad_method()),
        },
        "/api/runtime/target" => match method {
            Method::Get => Route::RuntimeTargetGet,
            Method::Put => Route::RuntimeTargetPut,
            _ => return Err(bad_method()),
        },
        "/api/runtime/coeff" => match method {
            Method::Get => Route::RuntimeCoeffGet,
            Method::Put => Route::RuntimeCoeffPut,
            _ => return Err(bad_method()),
        },
        "/api/mode" => match method {
            Method::Get => Route::LegacyModeGet,
            Method::Put => Route::LegacyModePut,
            _ => return Err(bad_method()),
        },
        "/api/mode/script-status" => match method {
            Method::Get => Route::LegacyModeScriptStatus,
            _ => return Err(bad_method()),
        },
        "/api/audio/format" => match method {
            Method::Get => Route::AudioFormatGet,
            Method::Put => Route::AudioFormatPut,
            _ => return Err(bad_method()),
        },
        "/api/channelmap/input-mode" => match method {
            Method::Get => Route::InputModeGet,
            Method::Put => Route::InputModePut,
            _ => return Err(bad_method()),
        },
        "/api/calibration/jobs" => match method {
            Method::Get => Route::CalJobsList,
            Method::Post => Route::CalJobsStart,
            _ => return Err(bad_method()),
        },
        "/api/calibration/settings" => match method {
            Method::Get => Route::CalSettingsGet,
            Method::Post => Route::CalSettingsPost,
            _ => return Err(bad_method()),
        },
        "/api/calibration/settings/cal-file" => match method {
            Method::Post => Route::CalSettingsCalFilePost,
            _ => return Err(bad_method()),
        },
        "/api/calibration/status" => match method {
            Method::Get => Route::CalStatusGet,
            _ => return Err(bad_method()),
        },
        "/api/calibration/apply" => match method {
            Method::Post => Route::CalApplyPost,
            _ => return Err(bad_method()),
        },
        "/api/calibration/clear" => match method {
            Method::Post => Route::CalClearPost,
            _ => return Err(bad_method()),
        },
        "/api/calibration/run" => match method {
            Method::Post => Route::CalRunPost,
            _ => return Err(bad_method()),
        },
        _ => {
            if let Some((job_id, action)) = cal_job_path(&path) {
                match (method, action.as_deref()) {
                    (Method::Get, None) => Route::CalJobGet(job_id),
                    (Method::Post, Some("cancel")) => Route::CalJobCancel(job_id),
                    (Method::Post, Some("apply")) => Route::CalJobApply(job_id),
                    (Method::Post, Some("retest")) => Route::CalJobRetest(job_id),
                    (Method::Post, Some("rollback")) => Route::CalJobRollback(job_id),
                    _ => return Err(bad_method()),
                }
            } else {
                return Err(RestError::not_found(format!("unknown API path '{path}'")));
            }
        }
    };
    Ok(route)
}

/// X14/R50：可选 API 版本头；缺省 = v1，仅显式不兼容版本 400。
fn check_api_version(request: &RestRequest) -> Result<(), RestError> {
    if let Some(raw) = request.header_value(API_VERSION_HEADER) {
        let value = raw.trim();
        let compatible = value.is_empty() || value == API_VERSION_RESPONSE_VALUE || value == "v1";
        if !compatible {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_API_VERSION,
                format!(
                    "unsupported API version '{value}'; supported: {API_VERSION_RESPONSE_VALUE}"
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 鉴权（R02-R06/K03/X21）：会话 + token fallback + CSRF
// ---------------------------------------------------------------------------

fn session_cookie_header(session_id: &str, ttl_secs: u64) -> (String, String) {
    (
        "Set-Cookie".to_string(),
        format!(
            "{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={ttl_secs}"
        ),
    )
}

fn clear_session_cookie_header() -> (String, String) {
    (
        "Set-Cookie".to_string(),
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    )
}

fn purge_expired_sessions(inner: &mut Inner, now: u64) {
    let expired: Vec<String> = inner
        .auth
        .sessions
        .iter()
        .filter(|(_, session)| session.expires_unix <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        inner.auth.sessions.remove(&id);
    }
    let sessions = &inner.auth.sessions;
    inner
        .auth
        .session_order
        .retain(|id| sessions.contains_key(id));
}

/// 新建会话；超过 MAX_SESSIONS 时淘汰最旧会话（R05：有界）。
fn create_session(inner: &mut Inner, now: u64) -> (String, String, u64) {
    purge_expired_sessions(inner, now);
    while inner.auth.sessions.len() >= inner.config.max_sessions.max(1) {
        let Some(oldest) = inner.auth.session_order.pop_front() else {
            break;
        };
        inner.auth.sessions.remove(&oldest);
    }
    let session_id = random_hex_16();
    let csrf = random_hex_16();
    let ttl = inner.config.session_ttl_secs;
    inner.auth.sessions.insert(
        session_id.clone(),
        Session {
            csrf: csrf.clone(),
            created_unix: now,
            expires_unix: now.saturating_add(ttl),
        },
    );
    inner.auth.session_order.push_back(session_id.clone());
    (session_id, csrf, ttl)
}

/// 从请求提取直接 API token（X-TSPI-Token / S1 遗留 X-Auth-Token / Bearer）。
fn request_token(request: &RestRequest) -> Option<&str> {
    for name in TOKEN_HEADERS {
        if let Some(value) = request.header_value(name) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    if let Some(raw) = request.header_value(AUTHORIZATION_HEADER) {
        let raw = raw.trim();
        for prefix in ["Bearer ", "bearer "] {
            if let Some(rest) = raw.strip_prefix(prefix) {
                let rest = rest.trim();
                if !rest.is_empty() {
                    return Some(rest);
                }
            }
        }
    }
    None
}

/// 认证（R04/R05/K03）：先 token（免 CSRF），再 Cookie 会话；都不行 401。
fn authenticate(
    inner: &mut Inner,
    request: &RestRequest,
    now: u64,
) -> Result<AuthContext, RestError> {
    if let Some(provided) = request_token(request) {
        let expected = inner.auth.token.as_deref().unwrap_or_default();
        if !expected.is_empty() && constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
            return Ok(AuthContext {
                kind: AuthKind::HeaderToken,
                session_id: None,
                csrf: None,
            });
        }
        return Err(RestError::unauthorized());
    }
    if let Some(session_id) = request.cookie_value(SESSION_COOKIE) {
        purge_expired_sessions(inner, now);
        return match inner.auth.sessions.get(session_id) {
            Some(session) => Ok(AuthContext {
                kind: AuthKind::Session,
                session_id: Some(session_id.to_string()),
                csrf: Some(session.csrf.clone()),
            }),
            None => Err(RestError::unauthorized()),
        };
    }
    Err(RestError::unauthorized())
}

/// K03：Cookie 会话的写方法必须带 CSRF；header token 免（显式凭证不依赖浏览器环境）。
fn require_csrf(context: &AuthContext, request: &RestRequest) -> Result<(), RestError> {
    if context.kind != AuthKind::Session {
        return Ok(());
    }
    let expected = context.csrf.as_deref().unwrap_or_default();
    let provided = request
        .header_value(CSRF_HEADER)
        .or_else(|| request.header_value(CSRF_HEADER_ALIAS))
        .unwrap_or_default();
    if expected.is_empty() || !constant_time_eq(provided.trim().as_bytes(), expected.as_bytes()) {
        return Err(RestError::forbidden("missing or invalid CSRF token"));
    }
    Ok(())
}

fn validate_new_password(inner: &Inner, password: &str, confirm: &str) -> Result<(), RestError> {
    if password.len() < inner.config.min_password_len {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            format!(
                "password must be at least {} characters",
                inner.config.min_password_len
            ),
        ));
    }
    if password.len() > 128 {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "password must be at most 128 characters",
        ));
    }
    if password.chars().all(char::is_whitespace) {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "password must not be blank",
        ));
    }
    if password != confirm {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "password confirmation does not match",
        ));
    }
    if let Some(token) = &inner.auth.token {
        if constant_time_eq(password.as_bytes(), token.as_bytes()) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "password must differ from the API token",
            ));
        }
    }
    Ok(())
}

fn login_response(session_id: &str, csrf: &str, ttl: u64) -> Result<RestResponse, RestError> {
    let (name, value) = session_cookie_header(session_id, ttl);
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("ok", JsonValue::Bool(true)),
            ("authenticated", JsonValue::Bool(true)),
            ("csrf_token", JsonValue::str(csrf)),
            ("expires_in", JsonValue::from(ttl)),
            ("auth_kind", JsonValue::str("session")),
        ]),
    )
    .with_header(&name, &value))
}

// ---------------------------------------------------------------------------
// R01-R06 handler
// ---------------------------------------------------------------------------

fn handle_auth_status(inner: &Inner) -> RestResponse {
    let password_configured = inner.auth.password.is_some();
    RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("password_configured", JsonValue::Bool(password_configured)),
            ("setup_required", JsonValue::Bool(!password_configured)),
            (
                "token_fallback",
                JsonValue::Bool(inner.auth.token.is_some()),
            ),
            ("mock", JsonValue::Bool(inner.config.mock)),
            (
                "min_password_len",
                JsonValue::from(inner.config.min_password_len),
            ),
            (
                "recommended_password_len",
                JsonValue::from(inner.config.recommended_password_len),
            ),
        ]),
    )
}

fn handle_auth_setup(inner: &mut Inner, request: &RestRequest) -> Result<RestResponse, RestError> {
    if inner.auth.password.is_some() {
        return Err(RestError::conflict(
            CODE_CONFLICT,
            "admin password is already configured",
        ));
    }
    let entries = parse_json_object_body(request, "/api/auth/setup", false)?;
    let mut fields = Fields::new(entries, "/api/auth/setup");
    let password = fields.take_str("password")?;
    let confirm = match fields.take_opt_str("confirm")? {
        Some(value) => value,
        None => password.clone(),
    };
    fields.finish()?;
    validate_new_password(inner, &password, &confirm)?;
    let iterations = inner.config.pbkdf2_iterations.max(1);
    inner.auth.password = Some(PasswordRecord::derive(&password, random_16(), iterations));
    let now = now_unix();
    let (session_id, csrf, ttl) = create_session(inner, now);
    login_response(&session_id, &csrf, ttl)
}

fn handle_auth_login(inner: &mut Inner, request: &RestRequest) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/auth/login", true)?;
    let mut fields = Fields::new(entries, "/api/auth/login");
    let password = fields
        .take_opt_str("password")?
        .filter(|value| !value.is_empty());
    let token = fields
        .take_opt_str("token")?
        .filter(|value| !value.is_empty());
    fields.finish()?;
    let now = now_unix();
    if inner.auth.lockout_until > now {
        return Err(RestError::rate_limited(inner.auth.lockout_until - now));
    }
    let authenticated = match (password, token) {
        (Some(_), Some(_)) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "provide exactly one of password or token",
            ))
        }
        (None, None) => {
            return Err(RestError::bad_request(
                CODE_MISSING_FIELD,
                "missing required field password or token in /api/auth/login",
            ))
        }
        (Some(password), None) => match inner.auth.password.as_ref() {
            Some(record) => record.verify(&password),
            None => {
                return Err(RestError::conflict(
                    CODE_CONFLICT,
                    "admin password is not configured; use /api/auth/setup",
                ))
            }
        },
        (None, Some(token)) => match inner.auth.token.as_deref() {
            Some(expected) if !expected.is_empty() => {
                constant_time_eq(token.as_bytes(), expected.as_bytes())
            }
            _ => false,
        },
    };
    if !authenticated {
        inner.auth.failed_logins = inner.auth.failed_logins.saturating_add(1);
        let limit = inner.config.max_failed_logins.max(1);
        if inner.auth.failed_logins >= limit {
            inner.auth.failed_logins = 0;
            inner.auth.lockout_until = now.saturating_add(inner.config.lockout_secs);
            return Err(RestError::rate_limited(inner.config.lockout_secs));
        }
        return Err(RestError::unauthorized());
    }
    inner.auth.failed_logins = 0;
    inner.auth.lockout_until = 0;
    let (session_id, csrf, ttl) = create_session(inner, now);
    login_response(&session_id, &csrf, ttl)
}

fn handle_auth_session(inner: &Inner, context: &AuthContext) -> RestResponse {
    let now = now_unix();
    let (expires_in, created_unix) = match &context.session_id {
        Some(session_id) => match inner.auth.sessions.get(session_id) {
            Some(session) => (
                session.expires_unix.saturating_sub(now),
                Some(session.created_unix),
            ),
            None => (0, None),
        },
        None => (inner.config.session_ttl_secs, None),
    };
    RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("authenticated", JsonValue::Bool(true)),
            ("auth_kind", JsonValue::str(context.kind.as_str())),
            (
                "csrf_token",
                match &context.csrf {
                    Some(csrf) => JsonValue::str(csrf.as_str()),
                    None => JsonValue::Null,
                },
            ),
            ("expires_in", JsonValue::from(expires_in)),
            (
                "created_unix",
                match created_unix {
                    Some(value) => JsonValue::from(value),
                    None => JsonValue::Null,
                },
            ),
        ]),
    )
}

fn handle_auth_logout(inner: &mut Inner, context: &AuthContext) -> Result<RestResponse, RestError> {
    if let Some(session_id) = &context.session_id {
        inner.auth.sessions.remove(session_id);
        let sessions = &inner.auth.sessions;
        inner
            .auth
            .session_order
            .retain(|id| sessions.contains_key(id));
    }
    let (name, value) = clear_session_cookie_header();
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("ok", JsonValue::Bool(true)),
            ("message", JsonValue::str("logged out")),
        ]),
    )
    .with_header(&name, &value))
}

// ---------------------------------------------------------------------------
// 受保护路由分发（R48/R49 与兼容面）
// ---------------------------------------------------------------------------

fn dispatch_protected(
    inner: &mut Inner,
    route: Route,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    match route {
        Route::RuntimeStatus => handle_runtime_status(inner),
        Route::RuntimeConfigGet => handle_runtime_config_get(inner),
        Route::RuntimeConfigPut => handle_runtime_config_put(inner, request),
        Route::RuntimePrepare => handle_runtime_prepare(inner, request),
        Route::RuntimeCommit => handle_runtime_commit(inner, request),
        Route::RuntimeAbort => handle_runtime_abort(inner, request),
        Route::RuntimeVolumeGet => handle_runtime_volume_get(inner),
        Route::RuntimeVolumePut => handle_runtime_volume_put(inner, request),
        Route::RuntimeMuteGet => handle_runtime_mute_get(inner),
        Route::RuntimeMutePut => handle_runtime_mute_put(inner, request),
        Route::RuntimeModeGet => handle_runtime_mode_get(inner),
        Route::RuntimeModePut => handle_runtime_mode_put(inner, request),
        Route::RuntimeTargetGet => handle_runtime_target_get(inner),
        Route::RuntimeTargetPut => handle_runtime_target_put(inner, request),
        Route::RuntimeCoeffGet => handle_runtime_coeff_get(inner),
        Route::RuntimeCoeffPut => handle_runtime_coeff_put(inner, request),
        Route::LegacyModeGet => handle_legacy_mode_get(inner),
        Route::LegacyModePut => handle_legacy_mode_put(inner, request),
        Route::LegacyModeScriptStatus => handle_legacy_mode_script_status(inner),
        Route::AudioFormatGet => handle_audio_format_get(inner),
        Route::AudioFormatPut => handle_audio_format_put(inner, request),
        Route::InputModeGet => handle_input_mode_get(),
        Route::InputModePut => handle_input_mode_put(),
        Route::CalJobsList => handle_cal_jobs_list(inner),
        Route::CalJobsStart => handle_cal_jobs_start(inner, request),
        Route::CalJobGet(job_id) => handle_cal_job_get(inner, &job_id),
        Route::CalJobCancel(job_id) => handle_cal_job_cancel(inner, &job_id),
        Route::CalJobApply(job_id) => handle_cal_job_apply(inner, &job_id),
        Route::CalJobRetest(job_id) => handle_cal_job_retest(inner, &job_id),
        Route::CalJobRollback(job_id) => handle_cal_job_rollback(inner, &job_id),
        Route::CalSettingsGet => handle_cal_settings_get(inner),
        Route::CalSettingsPost => handle_cal_settings_post(inner, request),
        Route::CalSettingsCalFilePost => handle_cal_settings_cal_file(),
        Route::CalStatusGet => handle_cal_status_get(inner),
        Route::CalApplyPost => handle_cal_apply(inner, request),
        Route::CalClearPost => handle_cal_clear(inner, request),
        Route::CalRunPost => handle_cal_run(inner, request),
        Route::Retired { replacement } => Err(RestError::retired(&replacement)),
        _ => Err(RestError::internal("route/auth classification mismatch")),
    }
}

// ---------------------------------------------------------------------------
// 通用小工具（ack 校验 / 幂等记录 / JSON 投影）
// ---------------------------------------------------------------------------

fn finite_or_null(value: f64) -> JsonValue {
    if value.is_finite() {
        JsonValue::from(value)
    } else {
        JsonValue::Null
    }
}

fn opt_string_json(value: &Option<String>) -> JsonValue {
    match value {
        Some(text) => JsonValue::str(text.as_str()),
        None => JsonValue::Null,
    }
}

fn remember_invalidated(inner: &mut Inner, prepare_id: String) {
    inner.invalidated.insert(prepare_id);
    while inner.invalidated.len() > MAX_INVALIDATED_PREPARES {
        let Some(first) = inner.invalidated.iter().next().cloned() else {
            break;
        };
        inner.invalidated.remove(&first);
    }
}

/// 作废当前 outstanding prepare（abort staged plan + 记入 stale 集合）。
fn invalidate_prepared_plan(inner: &mut Inner) {
    if let Some(prepared) = inner.prepared.take() {
        let _ = inner.engine.abort(prepared.engine_token);
        remember_invalidated(inner, prepared.prepare_id);
    }
}

fn remember_committed(inner: &mut Inner, prepare_id: &str, commit: &CommitOutcome) {
    inner.committed.insert(
        prepare_id.to_string(),
        CommitAck {
            actual_version: commit.actual_version.clone(),
            coeff: commit.coeff.clone(),
            target_hash: commit.target_hash.clone(),
            epoch: commit.epoch,
            committed_unix: commit.committed_unix,
            ack: commit.ack.clone(),
        },
    );
    inner.commit_order.push_back(prepare_id.to_string());
    while inner.commit_order.len() > MAX_COMMIT_REPLAYS {
        let Some(oldest) = inner.commit_order.pop_front() else {
            break;
        };
        inner.committed.remove(&oldest);
    }
}

/// prepare ack 合法性（坏 engine 值 -> 500，不把坏值当成功）。
fn validate_prepare_outcome(
    outcome: &PrepareOutcome,
    admitted: &RuntimeConfig,
) -> Result<(), RestError> {
    if outcome.planned_version.trim().is_empty()
        || outcome.planned_version.len() > MAX_VERSION_CHARS
    {
        return Err(RestError::internal(
            "engine prepare returned an invalid planned_version",
        ));
    }
    if !is_lower_hex64(&outcome.planned_target_hash) {
        return Err(RestError::internal(
            "engine prepare returned an invalid planned target hash",
        ));
    }
    if outcome.planned_target_hash != admitted.target.fingerprint() {
        return Err(RestError::internal(
            "engine prepare planned target hash does not match the admitted config",
        ));
    }
    if outcome.planned_coeff != admitted.coeff {
        return Err(RestError::internal(
            "engine prepare planned coeff does not match the admitted config",
        ));
    }
    if let Some(coeff) = &outcome.planned_coeff {
        coeff.validate().map_err(|_| {
            RestError::internal("engine prepare returned an invalid coeff reference")
        })?;
    }
    if outcome.admission.len() > MAX_ADMISSION_TEXT || outcome.admission.contains('\n') {
        return Err(RestError::internal(
            "engine prepare returned an invalid admission summary",
        ));
    }
    // T24 同口径 prepare 凭据：WS 侧同一校验函数；坏凭据 -> 500（不假 ack）。
    outcome
        .prepared
        .validate()
        .map_err(|error| ws_error_to_rest(&error))?;
    if outcome.prepared.config_sha256_hex != admitted.fingerprint() {
        return Err(RestError::internal(
            "engine prepare config_sha256 does not match the admitted config",
        ));
    }
    if outcome.prepared.admission_note != outcome.admission {
        return Err(RestError::internal(
            "engine prepare admission_note does not match the admission summary",
        ));
    }
    Ok(())
}

/// commit ack 合法性（R49/K15/K56：actual 版本/图 hash/目标/系数必须自洽）。
///
/// T24 交付后复用 ws::validate_commit_ack（plan_version/graph/config hash/expected_workers/
/// blocks_committed>=1/ack_mask==ACK_MASK 全部核对）；失败按 ws_error_to_rest 映射为 500
/// internal_error（commit_not_acked），绝不假成功。REST 另核对 display 字段与 prepare 一致。
fn validate_commit_ack(
    prepared: &ws::PreparedConfig,
    planned_target_hash: &str,
    planned_coeff: &Option<CoeffRef>,
    commit: &CommitOutcome,
) -> Result<(), RestError> {
    ws::validate_commit_ack(prepared, &commit.ack).map_err(|error| ws_error_to_rest(&error))?;
    if commit.actual_version.trim().is_empty() || commit.actual_version.len() > MAX_VERSION_CHARS {
        return Err(RestError::internal(
            "engine returned an invalid actual_version in the commit ack",
        ));
    }
    if !is_lower_hex64(&commit.ack.graph_id_hex) {
        return Err(RestError::internal(
            "engine returned an invalid graph_id in the commit ack",
        ));
    }
    if commit.target_hash != planned_target_hash {
        return Err(RestError::internal(
            "engine commit ack target hash does not match the prepared plan",
        ));
    }
    if &commit.coeff != planned_coeff {
        return Err(RestError::internal(
            "engine commit ack coeff does not match the prepared plan",
        ));
    }
    if let Some(coeff) = &commit.coeff {
        coeff
            .validate()
            .map_err(|_| RestError::internal("engine returned an invalid coeff reference"))?;
    }
    if commit.epoch == 0 {
        return Err(RestError::internal(
            "engine returned epoch 0 in the commit ack",
        ));
    }
    Ok(())
}

fn validate_volume_ack(ack: &VolumeAck) -> Result<(), RestError> {
    if !ack.volume_db.is_finite() || ack.volume_db < VOLUME_MIN_DB || ack.volume_db > VOLUME_MAX_DB
    {
        return Err(RestError::internal(
            "engine returned an out-of-range volume ack",
        ));
    }
    Ok(())
}

fn validate_mute_ack(ack: &MuteAck) -> Result<(), RestError> {
    if !ack.volume_db.is_finite() || ack.volume_db < VOLUME_MIN_DB || ack.volume_db > VOLUME_MAX_DB
    {
        return Err(RestError::internal(
            "engine returned an out-of-range volume in the mute ack",
        ));
    }
    Ok(())
}

fn validate_mode_ack(ack: &ModeAck, requested: ControlMode) -> Result<(), RestError> {
    if ack.mode != requested {
        return Err(RestError::internal(
            "engine switch_mode ack does not match the requested mode",
        ));
    }
    if !matches!(
        ack.switch_state.as_str(),
        "switched" | "switching" | "pending"
    ) {
        return Err(RestError::internal(
            "engine switch_mode ack has an invalid switch_state",
        ));
    }
    if !SAMPLE_RATES_HZ.contains(&ack.sample_rate) {
        return Err(RestError::internal(
            "engine switch_mode ack has an unsupported sample rate",
        ));
    }
    if !BLOCK_SIZES.contains(&ack.chunksize) {
        return Err(RestError::internal(
            "engine switch_mode ack has an unsupported chunksize",
        ));
    }
    if ack.target_hash.trim().is_empty() || ack.target_hash.len() > MAX_VERSION_CHARS {
        return Err(RestError::internal(
            "engine switch_mode ack has an invalid target hash",
        ));
    }
    Ok(())
}

/// 单次 prepare+commit（兼容面 PUT 用：audio/format 等必须真实生效才回 200）。
fn apply_config(inner: &mut Inner, candidate: RuntimeConfig) -> Result<CommitOutcome, RestError> {
    if let Some(coeff) = &candidate.coeff {
        if !inner.engine.coeff_known(coeff) {
            return Err(RestError::bad_request(
                CODE_UNKNOWN_COEFF,
                format!("coeff version '{}' is not available", coeff.version),
            ));
        }
    }
    let outcome = inner
        .engine
        .prepare(&candidate)
        .map_err(rt_status_to_error)?;
    let commit = match inner.engine.commit(outcome.engine_token) {
        Ok(commit) => commit,
        Err(status) => {
            let _ = inner.engine.abort(outcome.engine_token);
            return Err(rt_status_to_error(status));
        }
    };
    validate_commit_ack(
        &outcome.prepared,
        &outcome.planned_target_hash,
        &outcome.planned_coeff,
        &commit,
    )?;
    inner.desired = candidate;
    invalidate_prepared_plan(inner);
    Ok(commit)
}

impl From<u8> for JsonValue {
    fn from(value: u8) -> Self {
        JsonValue::Num(f64::from(value))
    }
}

// ---------------------------------------------------------------------------
// R49：runtime 状态与配置
// ---------------------------------------------------------------------------

/// desired 配置覆盖（PUT config / prepare 复用；未知字段由调用方 finish 拒收）。
fn apply_config_overrides(
    fields: &mut Fields,
    config: &mut RuntimeConfig,
) -> Result<bool, RestError> {
    let mut changed = false;
    if let Some(value) = fields.take("mode") {
        config.mode = parse_mode_value(&value)?;
        changed = true;
    }
    if let Some(value) = fields.take_opt_u32("sample_rate")? {
        config.sample_rate = value;
        changed = true;
    }
    if let Some(value) = fields.take_opt_u16("chunksize")? {
        config.chunksize = value;
        changed = true;
    }
    if let Some(value) = fields.take("target") {
        config.target = TargetSpec::from_json(&value, "target")?;
        changed = true;
    }
    match fields.take("coeff") {
        Some(JsonValue::Null) => {
            config.coeff = None;
            changed = true;
        }
        Some(value) => {
            config.coeff = Some(CoeffRef::from_json(&value, "coeff")?);
            changed = true;
        }
        None => {}
    }
    Ok(changed)
}

fn parse_mode_value(value: &JsonValue) -> Result<ControlMode, RestError> {
    match value {
        JsonValue::Str(text) => ControlMode::parse(text).ok_or_else(|| {
            RestError::bad_request(
                CODE_UNKNOWN_MODE,
                format!(
                    "unknown mode '{text}'; allowed: {}",
                    ALLOWED_MODES.join(",")
                ),
            )
        }),
        _ => Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "mode must be a string",
        )),
    }
}

fn actual_config(snapshot: &EngineSnapshot) -> RuntimeConfig {
    RuntimeConfig {
        mode: snapshot.mode,
        sample_rate: snapshot.sample_rate,
        chunksize: snapshot.chunksize,
        target: snapshot.target.clone(),
        coeff: snapshot.coeff.clone(),
    }
}

fn status_body(inner: &Inner, snapshot: &EngineSnapshot) -> JsonValue {
    let available_coeffs = snapshot
        .available_coeffs
        .iter()
        .map(CoeffRef::to_json)
        .collect();
    let info = inner.engine.engine_info();
    JsonValue::obj(vec![
        ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
        ("api_version", JsonValue::from(API_VERSION)),
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("actual", JsonValue::Bool(info.is_actual())),
        ("mode_controller", JsonValue::Bool(info.mode_controller)),
        ("mode_services", JsonValue::str(info.mode_services)),
        (
            "runtime_state",
            JsonValue::str(snapshot.runtime_state.as_str()),
        ),
        (
            "device_owner",
            JsonValue::str(snapshot.device_owner.as_str()),
        ),
        ("epoch", JsonValue::from(snapshot.epoch)),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        ("graph_id", JsonValue::str(snapshot.graph_id.as_str())),
        ("coeff", coeff_option_json(&snapshot.coeff)),
        (
            "last_good_coeff",
            coeff_option_json(&snapshot.last_good_coeff),
        ),
        ("available_coeffs", JsonValue::Arr(available_coeffs)),
        ("target", snapshot.target.to_json()),
        ("target_hash", JsonValue::str(snapshot.target_hash.as_str())),
        ("mode", JsonValue::str(snapshot.mode.as_str())),
        ("mode_label", JsonValue::str(snapshot.mode.label())),
        ("mode_profile", opt_string_json(&snapshot.mode_profile)),
        ("sample_rate", JsonValue::from(snapshot.sample_rate)),
        ("chunksize", JsonValue::from(snapshot.chunksize)),
        ("volume_db", finite_or_null(snapshot.volume_db)),
        ("mute", JsonValue::Bool(snapshot.mute)),
        ("xruns", JsonValue::from(snapshot.xruns)),
        ("deadline_misses", JsonValue::from(snapshot.deadline_misses)),
        ("clipped_samples", JsonValue::from(snapshot.clipped_samples)),
        (
            "calibration_owner",
            JsonValue::Bool(snapshot.calibration_owner),
        ),
        ("committed_unix", JsonValue::from(snapshot.committed_unix)),
        (
            "pending_prepare",
            match &inner.prepared {
                Some(prepared) => JsonValue::str(prepared.prepare_id.as_str()),
                None => JsonValue::Null,
            },
        ),
        (
            "pending_plan",
            match &inner.prepared {
                Some(prepared) => JsonValue::obj(vec![
                    ("prepare_id", JsonValue::str(prepared.prepare_id.as_str())),
                    (
                        "planned_version",
                        JsonValue::str(prepared.planned_version.as_str()),
                    ),
                    ("admission", JsonValue::str(prepared.admission.as_str())),
                    ("created_unix", JsonValue::from(prepared.created_unix)),
                    ("invalidated", JsonValue::Bool(prepared.invalidated)),
                ]),
                None => JsonValue::Null,
            },
        ),
    ])
}

fn config_body(inner: &Inner, snapshot: &EngineSnapshot, config: &RuntimeConfig) -> JsonValue {
    let applied = actual_config(snapshot);
    let requires_commit = config.fingerprint() != applied.fingerprint();
    JsonValue::obj(vec![
        ("schema", JsonValue::str(CONFIG_SCHEMA)),
        ("config", config.to_json()),
        ("config_hash", JsonValue::str(config.fingerprint())),
        ("actual_config", applied.to_json()),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        ("graph_id", JsonValue::str(snapshot.graph_id.as_str())),
        ("epoch", JsonValue::from(snapshot.epoch)),
        ("requires_commit", JsonValue::Bool(requires_commit)),
        (
            "pending_prepare",
            match &inner.prepared {
                Some(prepared) => JsonValue::str(prepared.prepare_id.as_str()),
                None => JsonValue::Null,
            },
        ),
    ])
}

fn handle_runtime_status(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(200, &status_body(inner, &snapshot)))
}

fn handle_runtime_config_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    let config = inner.desired.clone();
    Ok(RestResponse::json(
        200,
        &config_body(inner, &snapshot, &config),
    ))
}

fn handle_runtime_config_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/config", false)?;
    let mut fields = Fields::new(entries, "/api/runtime/config");
    let mut candidate = inner.desired.clone();
    let changed = apply_config_overrides(&mut fields, &mut candidate)?;
    fields.finish()?;
    if !changed {
        return Err(RestError::bad_request(
            CODE_MISSING_FIELD,
            "at least one config field (mode/sample_rate/chunksize/target/coeff) is required",
        ));
    }
    candidate.validate()?;
    if let Some(coeff) = &candidate.coeff {
        if !inner.engine.coeff_known(coeff) {
            return Err(RestError::bad_request(
                CODE_UNKNOWN_COEFF,
                format!("coeff version '{}' is not available", coeff.version),
            ));
        }
    }
    // 配置意图更新不改变输出；真实生效必须显式 prepare/commit（R49 prepare 与 commit 分离）。
    inner.desired = candidate.clone();
    invalidate_prepared_plan(inner);
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        200,
        &config_body(inner, &snapshot, &candidate),
    ))
}

// ---------------------------------------------------------------------------
// R49：prepare -> commit 两阶段与 actual ack（不假 ack）
// ---------------------------------------------------------------------------

fn commit_body(
    inner: &Inner,
    prepare_id: &str,
    commit: &CommitOutcome,
    replayed: bool,
) -> JsonValue {
    let snapshot = inner.engine.snapshot();
    let info = inner.engine.engine_info();
    JsonValue::obj(vec![
        ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
        ("committed", JsonValue::Bool(true)),
        ("replayed", JsonValue::Bool(replayed)),
        // F-13：commit 回执必须显式声明 engine/synthetic；synthetic=true 时页面不得
        // 显示「已生效（actual）」。真实 host engine（T26）才返回 actual=true。
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("actual", JsonValue::Bool(info.is_actual())),
        ("prepare_id", JsonValue::str(prepare_id)),
        (
            "actual_version",
            JsonValue::str(commit.actual_version.as_str()),
        ),
        ("graph_id", JsonValue::str(commit.ack.graph_id_hex.as_str())),
        (
            "config_sha256",
            JsonValue::str(commit.ack.config_sha256_hex.as_str()),
        ),
        ("plan_version", JsonValue::from(commit.ack.plan_version)),
        (
            "previous_plan_version",
            JsonValue::from(commit.ack.previous_plan_version),
        ),
        (
            "expected_workers",
            JsonValue::from(commit.ack.expected_workers),
        ),
        (
            "blocks_committed",
            JsonValue::from(commit.ack.blocks_committed),
        ),
        ("ack_mask", JsonValue::from(commit.ack.ack_mask)),
        ("rolled_back", JsonValue::Bool(commit.ack.rolled_back)),
        ("coeff", coeff_option_json(&commit.coeff)),
        ("target_hash", JsonValue::str(commit.target_hash.as_str())),
        ("epoch", JsonValue::from(commit.epoch)),
        ("committed_unix", JsonValue::from(commit.committed_unix)),
        ("mode", JsonValue::str(snapshot.mode.as_str())),
        ("sample_rate", JsonValue::from(snapshot.sample_rate)),
        ("chunksize", JsonValue::from(snapshot.chunksize)),
        (
            "runtime_state",
            JsonValue::str(snapshot.runtime_state.as_str()),
        ),
        (
            "device_owner",
            JsonValue::str(snapshot.device_owner.as_str()),
        ),
    ])
}

fn commit_outcome_from_ack(ack: &CommitAck) -> CommitOutcome {
    CommitOutcome {
        actual_version: ack.actual_version.clone(),
        coeff: ack.coeff.clone(),
        target_hash: ack.target_hash.clone(),
        epoch: ack.epoch,
        committed_unix: ack.committed_unix,
        ack: ack.ack.clone(),
    }
}

fn handle_runtime_prepare(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/prepare", true)?;
    let mut candidate = inner.desired.clone();
    if !entries.is_empty() {
        let mut fields = Fields::new(entries, "/api/runtime/prepare");
        apply_config_overrides(&mut fields, &mut candidate)?;
        fields.finish()?;
    }
    candidate.validate()?;
    if let Some(coeff) = &candidate.coeff {
        if !inner.engine.coeff_known(coeff) {
            return Err(RestError::bad_request(
                CODE_UNKNOWN_COEFF,
                format!("coeff version '{}' is not available", coeff.version),
            ));
        }
    }
    // prepare 只做非 RT admission，不改变输出；失败保持旧图/旧 desired 不变。
    let outcome = inner
        .engine
        .prepare(&candidate)
        .map_err(rt_status_to_error)?;
    validate_prepare_outcome(&outcome, &candidate)?;
    invalidate_prepared_plan(inner);
    inner.next_prepare = inner.next_prepare.saturating_add(1);
    let prepare_id = format!("prep-{:04}", inner.next_prepare);
    inner.desired = candidate.clone();
    inner.prepared = Some(PreparedPlan {
        prepare_id: prepare_id.clone(),
        engine_token: outcome.engine_token,
        fingerprint: candidate.fingerprint(),
        planned_version: outcome.planned_version.clone(),
        planned_target_hash: outcome.planned_target_hash.clone(),
        planned_coeff: outcome.planned_coeff.clone(),
        admission: outcome.admission.clone(),
        created_unix: now_unix(),
        invalidated: false,
        prepared: outcome.prepared.clone(),
    });
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
            ("prepared", JsonValue::Bool(true)),
            ("engine", JsonValue::str(inner.engine.engine_info().engine)),
            (
                "synthetic",
                JsonValue::Bool(inner.engine.engine_info().synthetic),
            ),
            (
                "actual",
                JsonValue::Bool(inner.engine.engine_info().is_actual()),
            ),
            ("prepare_id", JsonValue::str(prepare_id)),
            (
                "planned_version",
                JsonValue::str(outcome.planned_version.as_str()),
            ),
            (
                "planned_target_hash",
                JsonValue::str(outcome.planned_target_hash.as_str()),
            ),
            (
                "planned_config_hash",
                JsonValue::str(candidate.fingerprint().as_str()),
            ),
            ("planned_coeff", coeff_option_json(&outcome.planned_coeff)),
            ("admission", JsonValue::str(outcome.admission.as_str())),
            (
                "prepared_plan_version",
                JsonValue::from(outcome.prepared.plan_version),
            ),
            (
                "prepared_graph_id",
                JsonValue::str(outcome.prepared.graph_id_hex.as_str()),
            ),
            (
                "prepared_config_sha256",
                JsonValue::str(outcome.prepared.config_sha256_hex.as_str()),
            ),
            (
                "expected_workers",
                JsonValue::from(outcome.prepared.expected_workers),
            ),
            (
                "admitted_bytes",
                JsonValue::from(outcome.prepared.admitted_bytes),
            ),
            (
                "actual_version",
                JsonValue::str(snapshot.actual_version.as_str()),
            ),
            ("requires_commit", JsonValue::Bool(true)),
        ]),
    ))
}

fn handle_runtime_commit(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/commit", false)?;
    let mut fields = Fields::new(entries, "/api/runtime/commit");
    let prepare_id = fields.take_opt_str("prepare_id")?;
    fields.finish()?;
    let prepare_id = prepare_id.ok_or_else(|| {
        RestError::bad_request(
            CODE_MISSING_FIELD,
            "missing required field 'prepare_id' in /api/runtime/commit",
        )
    })?;
    // 幂等：同一 prepare_id 重复 commit 返回同一 actual ack（R49）。
    if let Some(ack) = inner.committed.get(&prepare_id) {
        let outcome = commit_outcome_from_ack(ack);
        return Ok(RestResponse::json(
            200,
            &commit_body(inner, &prepare_id, &outcome, true),
        ));
    }
    if inner.invalidated.contains(&prepare_id) {
        return Err(RestError::conflict(
            CODE_STALE,
            "prepare_id has been invalidated; re-prepare",
        ));
    }
    let prepared = match inner.prepared.take() {
        Some(prepared) if prepared.prepare_id == prepare_id => prepared,
        Some(other) => {
            inner.prepared = Some(other);
            return Err(RestError::conflict(
                CODE_NOT_PREPARED,
                "prepare_id does not match the outstanding prepare",
            ));
        }
        None => {
            return Err(RestError::conflict(
                CODE_NOT_PREPARED,
                "no outstanding prepare; call /api/runtime/prepare first",
            ))
        }
    };
    if prepared.invalidated {
        remember_invalidated(inner, prepared.prepare_id);
        return Err(RestError::conflict(
            CODE_STALE,
            "prepare_id was invalidated; re-prepare",
        ));
    }
    if prepared.fingerprint != inner.desired.fingerprint() {
        remember_invalidated(inner, prepared.prepare_id);
        return Err(RestError::conflict(
            CODE_STALE,
            "desired config changed after prepare; re-prepare",
        ));
    }
    let commit = match inner.engine.commit(prepared.engine_token) {
        Ok(commit) => commit,
        Err(status) => {
            remember_invalidated(inner, prepared.prepare_id);
            return Err(rt_status_to_error(status));
        }
    };
    validate_commit_ack(
        &prepared.prepared,
        &prepared.planned_target_hash,
        &prepared.planned_coeff,
        &commit,
    )?;
    remember_committed(inner, &prepare_id, &commit);
    Ok(RestResponse::json(
        200,
        &commit_body(inner, &prepare_id, &commit, false),
    ))
}

fn handle_runtime_abort(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/abort", false)?;
    let mut fields = Fields::new(entries, "/api/runtime/abort");
    let prepare_id = fields.take_opt_str("prepare_id")?;
    fields.finish()?;
    let prepare_id = prepare_id.ok_or_else(|| {
        RestError::bad_request(
            CODE_MISSING_FIELD,
            "missing required field 'prepare_id' in /api/runtime/abort",
        )
    })?;
    if inner.committed.contains_key(&prepare_id) {
        return Err(RestError::conflict(
            CODE_CONFLICT,
            "prepare_id has already been committed; abort is not applicable",
        ));
    }
    if inner.invalidated.contains(&prepare_id) {
        return Ok(RestResponse::json(
            200,
            &JsonValue::obj(vec![
                ("aborted", JsonValue::Bool(true)),
                ("prepare_id", JsonValue::str(prepare_id)),
                ("already_invalidated", JsonValue::Bool(true)),
            ]),
        ));
    }
    let prepared = match inner.prepared.take() {
        Some(prepared) if prepared.prepare_id == prepare_id => prepared,
        Some(other) => {
            inner.prepared = Some(other);
            return Err(RestError::conflict(
                CODE_NOT_PREPARED,
                "prepare_id does not match the outstanding prepare",
            ));
        }
        None => {
            return Err(RestError::conflict(
                CODE_NOT_PREPARED,
                "no outstanding prepare to abort",
            ))
        }
    };
    let status = inner.engine.abort(prepared.engine_token);
    remember_invalidated(inner, prepare_id.clone());
    if status.is_ok() {
        Ok(RestResponse::json(
            200,
            &JsonValue::obj(vec![
                ("aborted", JsonValue::Bool(true)),
                ("prepare_id", JsonValue::str(prepare_id)),
                ("already_invalidated", JsonValue::Bool(false)),
            ]),
        ))
    } else {
        Err(rt_status_to_error(status))
    }
}

// ---------------------------------------------------------------------------
// R49/K53：volume / mute（-150..+50 dB、mute 独立、actual 回读、幂等）
// ---------------------------------------------------------------------------

fn volume_body(inner: &Inner, snapshot: &EngineSnapshot) -> JsonValue {
    let info = inner.engine.engine_info();
    // F-13：volume 回读同样必须带 engine/synthetic，页面不得把内存替身写成 actual。
    JsonValue::obj(vec![
        ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("actual", JsonValue::Bool(info.is_actual())),
        ("volume_db", finite_or_null(snapshot.volume_db)),
        ("mute", JsonValue::Bool(snapshot.mute)),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        ("epoch", JsonValue::from(snapshot.epoch)),
        (
            "runtime_state",
            JsonValue::str(snapshot.runtime_state.as_str()),
        ),
    ])
}

fn handle_runtime_volume_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(200, &volume_body(inner, &snapshot)))
}

fn handle_runtime_volume_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/volume", false)?;
    let mut fields = Fields::new(entries, "/api/runtime/volume");
    let db = fields.take_opt_f64("db")?;
    let volume_db = fields.take_opt_f64("volume_db")?;
    fields.finish()?;
    let value = match (db, volume_db) {
        (Some(value), None) | (None, Some(value)) => value,
        (Some(_), Some(_)) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "provide exactly one of db or volume_db",
            ))
        }
        (None, None) => {
            return Err(RestError::bad_request(
                CODE_MISSING_FIELD,
                "missing required field 'db' (or 'volume_db') in /api/runtime/volume",
            ))
        }
    };
    if !value.is_finite() || value < VOLUME_MIN_DB || value > VOLUME_MAX_DB {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            format!("volume must be a finite number in [{VOLUME_MIN_DB}, {VOLUME_MAX_DB}] dB"),
        ));
    }
    let ack = inner.engine.set_volume(value).map_err(rt_status_to_error)?;
    validate_volume_ack(&ack)?;
    let snapshot = inner.engine.snapshot();
    let info = inner.engine.engine_info();
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("ok", JsonValue::Bool(true)),
            ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
            // F-13：volume 写回执显式带 engine/synthetic，不得冒充 actual。
            ("engine", JsonValue::str(info.engine)),
            ("synthetic", JsonValue::Bool(info.synthetic)),
            ("actual", JsonValue::Bool(info.is_actual())),
            ("volume_db", JsonValue::from(ack.volume_db)),
            ("mute", JsonValue::Bool(ack.mute)),
            ("requested_volume_db", JsonValue::from(value)),
            (
                "actual_version",
                JsonValue::str(snapshot.actual_version.as_str()),
            ),
            ("epoch", JsonValue::from(snapshot.epoch)),
        ]),
    ))
}

fn handle_runtime_mute_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    let info = inner.engine.engine_info();
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
            ("engine", JsonValue::str(info.engine)),
            ("synthetic", JsonValue::Bool(info.synthetic)),
            ("actual", JsonValue::Bool(info.is_actual())),
            ("mute", JsonValue::Bool(snapshot.mute)),
            ("volume_db", finite_or_null(snapshot.volume_db)),
            (
                "actual_version",
                JsonValue::str(snapshot.actual_version.as_str()),
            ),
            ("epoch", JsonValue::from(snapshot.epoch)),
            (
                "runtime_state",
                JsonValue::str(snapshot.runtime_state.as_str()),
            ),
        ]),
    ))
}

fn handle_runtime_mute_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/mute", false)?;
    let mut fields = Fields::new(entries, "/api/runtime/mute");
    let mute = fields.take_bool("mute")?;
    fields.finish()?;
    let ack = inner.engine.set_mute(mute).map_err(rt_status_to_error)?;
    validate_mute_ack(&ack)?;
    let snapshot = inner.engine.snapshot();
    let info = inner.engine.engine_info();
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("ok", JsonValue::Bool(true)),
            ("schema", JsonValue::str(RUNTIME_STATUS_SCHEMA)),
            // F-13：mute 写回执显式带 engine/synthetic，不得冒充 actual。
            ("engine", JsonValue::str(info.engine)),
            ("synthetic", JsonValue::Bool(info.synthetic)),
            ("actual", JsonValue::Bool(info.is_actual())),
            ("mute", JsonValue::Bool(ack.mute)),
            ("requested_mute", JsonValue::Bool(mute)),
            ("volume_db", JsonValue::from(ack.volume_db)),
            (
                "actual_version",
                JsonValue::str(snapshot.actual_version.as_str()),
            ),
            ("epoch", JsonValue::from(snapshot.epoch)),
        ]),
    ))
}

// ---------------------------------------------------------------------------
// R10/R11/A06（/api/mode）与 R49 runtime mode：202 接受、actual ack、状态机归 T44
// ---------------------------------------------------------------------------

fn mode_profiles_json() -> JsonValue {
    let items = ALLOWED_MODES
        .iter()
        .map(|name| {
            let mode = ControlMode::parse(name).expect("ALLOWED_MODES entry must parse");
            let format = if mode.is_playback() {
                "S24_3LE / 24-bit"
            } else {
                "-"
            };
            JsonValue::obj(vec![
                ("mode", JsonValue::str(mode.as_str())),
                ("label", JsonValue::str(mode.label())),
                ("input_format", JsonValue::str(format)),
                ("output_format", JsonValue::str(format)),
                ("default_chunksize", JsonValue::from(DEFAULT_CHUNKSIZE)),
                ("default_target_level", JsonValue::Null),
            ])
        })
        .collect();
    JsonValue::Arr(items)
}

fn legacy_mode_body(
    inner: &Inner,
    snapshot: &EngineSnapshot,
    pending: Option<ControlMode>,
    switch_state: &str,
    evidence: Option<&ModeStateEvidence>,
) -> JsonValue {
    let idle = switch_state == "idle";
    let info = inner.engine.engine_info();
    // F-28：mode_actual 只在「走 T44 状态机 + 真实 systemd 服务」时为 true；
    // 内存 engine / 模拟服务 / 未接线都保持 false（页面据此不写「已生效」）。
    let mode_actual = info.mode_controller && info.mode_services == "systemd";
    JsonValue::obj(vec![
        ("schema", JsonValue::str(MODE_STATUS_SCHEMA)),
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("actual", JsonValue::Bool(info.is_actual())),
        ("mode_controller", JsonValue::Bool(info.mode_controller)),
        ("mode_services", JsonValue::str(info.mode_services)),
        ("mode_actual", JsonValue::Bool(mode_actual)),
        (
            "mode_evidence",
            match evidence {
                Some(evidence) => JsonValue::obj(vec![
                    ("epoch", JsonValue::from(evidence.epoch)),
                    ("switch_seq", JsonValue::from(evidence.switch_seq)),
                    (
                        "servo_owner",
                        match &evidence.servo_owner {
                            Some(owner) => JsonValue::str(owner.as_str()),
                            None => JsonValue::Null,
                        },
                    ),
                    ("servo_claims", JsonValue::from(evidence.servo_claims)),
                    ("rolled_back", JsonValue::Bool(evidence.rolled_back)),
                    ("last_exit_code", JsonValue::from(evidence.last_exit_code)),
                    ("fallback", JsonValue::str(evidence.fallback)),
                ]),
                None => JsonValue::Null,
            },
        ),
        ("mode", JsonValue::str(snapshot.mode.as_str())),
        ("mode_label", JsonValue::str(snapshot.mode.label())),
        (
            "target",
            match pending {
                Some(mode) => JsonValue::str(mode.as_str()),
                None => JsonValue::Null,
            },
        ),
        (
            "target_label",
            match pending {
                Some(mode) => JsonValue::str(mode.label()),
                None => JsonValue::Null,
            },
        ),
        ("state", JsonValue::str(switch_state)),
        ("step", JsonValue::Null),
        ("step_label", JsonValue::Null),
        (
            "progress",
            JsonValue::from(if idle { 100u32 } else { 0u32 }),
        ),
        ("steps", JsonValue::Arr(Vec::new())),
        ("last_error", JsonValue::Null),
        ("warnings", JsonValue::Arr(Vec::new())),
        ("started_unix", JsonValue::Null),
        ("finished_unix", JsonValue::Null),
        ("enabled", JsonValue::Bool(inner.config.enable_mode_switch)),
        ("restore_on_start", JsonValue::Bool(false)),
        ("switch_script", JsonValue::str("")),
        ("strategy", JsonValue::str("internal")),
        ("profiles", mode_profiles_json()),
        (
            "script_status",
            JsonValue::obj(vec![
                ("available", JsonValue::Bool(false)),
                ("switch_script", JsonValue::str("")),
                (
                    "message",
                    JsonValue::str(
                        "external tspi-mode script retired in P1; mode switching is internal (T44)",
                    ),
                ),
            ]),
        ),
        (
            "persisted",
            JsonValue::obj(vec![
                ("mode", JsonValue::str(snapshot.mode.as_str())),
                ("mode_label", JsonValue::str(snapshot.mode.label())),
                ("updated_at_unix", JsonValue::from(snapshot.committed_unix)),
                ("last_error", JsonValue::Null),
                ("auto_restore", JsonValue::Bool(false)),
            ]),
        ),
        ("components", JsonValue::obj(Vec::new())),
        ("mock", JsonValue::Bool(inner.config.mock)),
        ("profile", opt_string_json(&snapshot.mode_profile)),
        ("layout", JsonValue::str(snapshot.target.layout.as_str())),
        ("target_layout", snapshot.target.to_json()),
        ("target_hash", JsonValue::str(snapshot.target_hash.as_str())),
        ("sample_rate", JsonValue::from(snapshot.sample_rate)),
        ("chunksize", JsonValue::from(snapshot.chunksize)),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        (
            "runtime_state",
            JsonValue::str(snapshot.runtime_state.as_str()),
        ),
        ("switch_state", JsonValue::str(switch_state)),
    ])
}

fn handle_runtime_mode_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    let evidence = inner.engine.mode_evidence();
    Ok(RestResponse::json(
        200,
        &legacy_mode_body(inner, &snapshot, None, "idle", evidence.as_ref()),
    ))
}

fn handle_legacy_mode_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    handle_runtime_mode_get(inner)
}

fn validate_mode_evidence(ack: &ModeAck, evidence: &ModeStateEvidence) -> Result<(), RestError> {
    // F-28：成功切换 = 新 epoch + 唯一 servo；任何一条不成立都按内部契约违例拒绝，
    // 不得把「switched」当成 actual 返回。
    if evidence.rolled_back {
        return Err(RestError::internal(
            "engine reported a successful mode switch with rolled_back evidence",
        ));
    }
    if evidence.epoch == 0 {
        return Err(RestError::internal(
            "engine switch_mode ack has no new epoch (ModeController not wired)",
        ));
    }
    if evidence.switch_seq == 0 {
        return Err(RestError::internal(
            "engine switch_mode ack has switch_seq 0 (ModeController not wired)",
        ));
    }
    if ack.mode.is_playback() && evidence.servo_owner.is_none() {
        return Err(RestError::internal(
            "engine switch_mode ack has no unique clock servo owner for a playback mode",
        ));
    }
    if ack.mode.is_playback() && evidence.servo_claims == 0 {
        return Err(RestError::internal(
            "engine switch_mode ack has no servo claim for a playback mode",
        ));
    }
    Ok(())
}

fn do_switch_mode(
    inner: &mut Inner,
    request: &RestRequest,
    path: &'static str,
) -> Result<(ModeAck, String, Option<ModeStateEvidence>), RestError> {
    if !inner.config.enable_mode_switch {
        return Err(RestError::forbidden(
            "mode switching is disabled in this deployment",
        ));
    }
    let entries = parse_json_object_body(request, path, false)?;
    let mut fields = Fields::new(entries, path);
    let mode_value = fields.take_required("mode")?;
    let mode = parse_mode_value(&mode_value)?;
    fields.finish()?;
    let ack = inner.engine.switch_mode(mode).map_err(rt_status_to_error)?;
    validate_mode_ack(&ack, mode)?;
    let evidence = inner.engine.mode_evidence();
    if let Some(evidence) = &evidence {
        validate_mode_evidence(&ack, evidence)?;
    }
    // N-01：切换成功后 actual 的 mode/rate/chunksize 已变，必须把 desired 同步到 actual。
    // 否则随后的 /api/audio/format 等 PUT 会基于旧 desired 再 prepare/commit，使 inner 实际
    // 提交与 controller 状态分叉（GET format 退回切换前的旧块长/旧 epoch）。
    if ack.switch_state == "switched" {
        let mut desired = inner.desired.clone();
        desired.mode = ack.mode;
        desired.sample_rate = ack.sample_rate;
        desired.chunksize = ack.chunksize;
        if desired != inner.desired {
            inner.desired = desired;
            // actual 已变：旧配置上的在途 prepare 作废（等价 host plan stale）。
            invalidate_prepared_plan(inner);
        }
    }
    let switch_state = match ack.switch_state.as_str() {
        "switched" => "idle",
        _ => "switching",
    }
    .to_string();
    Ok((ack, switch_state, evidence))
}

fn handle_runtime_mode_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let (ack, switch_state, evidence) = do_switch_mode(inner, request, "/api/runtime/mode")?;
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        202,
        &legacy_mode_body(
            inner,
            &snapshot,
            Some(ack.mode),
            &switch_state,
            evidence.as_ref(),
        ),
    ))
}

fn handle_legacy_mode_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let (ack, switch_state, evidence) = do_switch_mode(inner, request, "/api/mode")?;
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        202,
        &legacy_mode_body(
            inner,
            &snapshot,
            Some(ack.mode),
            &switch_state,
            evidence.as_ref(),
        ),
    ))
}

/// R12/K06：外部切换脚本已退役；GET 200 + available:false（不得 500、不得假装可用）。
fn handle_legacy_mode_script_status(inner: &Inner) -> Result<RestResponse, RestError> {
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("available", JsonValue::Bool(false)),
            ("switch_script", JsonValue::str("")),
            ("script", JsonValue::Null),
            ("mock", JsonValue::Bool(inner.config.mock)),
            (
                "message",
                JsonValue::str(
                    "external tspi-mode script retired in P1; mode switching is internal (T44)",
                ),
            ),
        ]),
    ))
}

// ---------------------------------------------------------------------------
// R49：target / coeff 版本（GET actual；PUT 只改 desired，需 prepare/commit 生效）
// ---------------------------------------------------------------------------

fn target_body(inner: &Inner, snapshot: &EngineSnapshot, target: &TargetSpec) -> JsonValue {
    let requires_commit = target.fingerprint() != snapshot.target.fingerprint();
    JsonValue::obj(vec![
        ("schema", JsonValue::str(CONFIG_SCHEMA)),
        ("target", target.to_json()),
        ("target_hash", JsonValue::str(target.fingerprint().as_str())),
        ("actual_target", snapshot.target.to_json()),
        (
            "actual_target_hash",
            JsonValue::str(snapshot.target_hash.as_str()),
        ),
        (
            "config_hash",
            JsonValue::str(inner.desired.fingerprint().as_str()),
        ),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        ("requires_commit", JsonValue::Bool(requires_commit)),
        (
            "pending_prepare",
            match &inner.prepared {
                Some(prepared) => JsonValue::str(prepared.prepare_id.as_str()),
                None => JsonValue::Null,
            },
        ),
    ])
}

fn handle_runtime_target_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    let target = inner.desired.target.clone();
    Ok(RestResponse::json(
        200,
        &target_body(inner, &snapshot, &target),
    ))
}

fn handle_runtime_target_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/target", false)?;
    let value = if entries.len() == 1 && entries[0].0 == "target" {
        entries[0].1.clone()
    } else {
        JsonValue::Obj(entries)
    };
    let target = TargetSpec::from_json(&value, "target")?;
    inner.desired.target = target.clone();
    invalidate_prepared_plan(inner);
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        200,
        &target_body(inner, &snapshot, &target),
    ))
}

fn coeff_body(inner: &Inner, snapshot: &EngineSnapshot, coeff: &Option<CoeffRef>) -> JsonValue {
    let available = snapshot
        .available_coeffs
        .iter()
        .map(CoeffRef::to_json)
        .collect();
    let requires_commit = coeff != &snapshot.coeff;
    JsonValue::obj(vec![
        ("schema", JsonValue::str(CONFIG_SCHEMA)),
        ("coeff", coeff_option_json(coeff)),
        ("actual_coeff", coeff_option_json(&snapshot.coeff)),
        (
            "last_good_coeff",
            coeff_option_json(&snapshot.last_good_coeff),
        ),
        ("available_coeffs", JsonValue::Arr(available)),
        (
            "config_hash",
            JsonValue::str(inner.desired.fingerprint().as_str()),
        ),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        ("requires_commit", JsonValue::Bool(requires_commit)),
        (
            "pending_prepare",
            match &inner.prepared {
                Some(prepared) => JsonValue::str(prepared.prepare_id.as_str()),
                None => JsonValue::Null,
            },
        ),
    ])
}

fn handle_runtime_coeff_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    let coeff = inner.desired.coeff.clone();
    Ok(RestResponse::json(
        200,
        &coeff_body(inner, &snapshot, &coeff),
    ))
}

fn handle_runtime_coeff_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/runtime/coeff", false)?;
    let coeff = if entries.len() == 1 && entries[0].0 == "coeff" {
        match &entries[0].1 {
            JsonValue::Null => None,
            value => Some(CoeffRef::from_json(value, "coeff")?),
        }
    } else {
        Some(CoeffRef::from_json(&JsonValue::Obj(entries), "coeff")?)
    };
    if let Some(reference) = &coeff {
        if !inner.engine.coeff_known(reference) {
            return Err(RestError::bad_request(
                CODE_UNKNOWN_COEFF,
                format!("coeff version '{}' is not available", reference.version),
            ));
        }
    }
    inner.desired.coeff = coeff.clone();
    invalidate_prepared_plan(inner);
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        200,
        &coeff_body(inner, &snapshot, &coeff),
    ))
}

// ---------------------------------------------------------------------------
// R13/R14：音频格式（runtime 仅 chunksize 64/128；显式拒绝不夹取）+ R47/C07
// ---------------------------------------------------------------------------

fn audio_format_body(inner: &Inner, snapshot: &EngineSnapshot) -> JsonValue {
    let playback_format = format!(
        "S24_3LE / {} Hz / {} ch",
        snapshot.sample_rate, snapshot.target.channels
    );
    let info = inner.engine.engine_info();
    // F-13：format GET 也是 actual 声明面；内存替身不得把自己写成 RT host actual。
    let source = if info.is_actual() {
        "internal RT host actual (T26)"
    } else {
        "synthetic memory engine (not RT host actual)"
    };
    JsonValue::obj(vec![
        ("schema", JsonValue::str(AUDIO_FORMAT_SCHEMA)),
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("actual", JsonValue::Bool(info.is_actual())),
        ("mode", JsonValue::str(snapshot.mode.as_str())),
        ("mode_label", JsonValue::str(snapshot.mode.label())),
        ("source", JsonValue::str(source)),
        ("sample_rate", JsonValue::from(snapshot.sample_rate)),
        ("bit_depth", JsonValue::from(24u32)),
        ("channels", JsonValue::from(snapshot.target.channels)),
        ("chunksize", JsonValue::from(snapshot.chunksize)),
        ("target_level", JsonValue::Null),
        ("input_format", JsonValue::str(playback_format.as_str())),
        ("output_format", JsonValue::str(playback_format.as_str())),
        ("config_path", JsonValue::Null),
        (
            "limits",
            JsonValue::obj(vec![
                (
                    "sample_rates",
                    JsonValue::Arr(
                        SAMPLE_RATES_HZ
                            .iter()
                            .map(|rate| JsonValue::from(*rate))
                            .collect(),
                    ),
                ),
                (
                    "chunksizes",
                    JsonValue::Arr(
                        BLOCK_SIZES
                            .iter()
                            .map(|size| JsonValue::from(*size))
                            .collect(),
                    ),
                ),
                (
                    "ui_reference_chunksizes",
                    JsonValue::Arr(vec![
                        JsonValue::from(32u16),
                        JsonValue::from(64u16),
                        JsonValue::from(128u16),
                        JsonValue::from(256u16),
                        JsonValue::from(512u16),
                        JsonValue::from(1024u16),
                    ]),
                ),
                ("default_sample_rate", JsonValue::from(DEFAULT_SAMPLE_RATE)),
                ("default_chunksize", JsonValue::from(DEFAULT_CHUNKSIZE)),
            ]),
        ),
        (
            "warnings",
            JsonValue::Arr(vec![JsonValue::str(
                "runtime v1 accepts chunksize 64/128 only; 32/256/512/1024 are UI reference values",
            )]),
        ),
        (
            "xrun",
            JsonValue::obj(vec![
                ("xruns", JsonValue::from(snapshot.xruns)),
                ("deadline_misses", JsonValue::from(snapshot.deadline_misses)),
                ("clipped_samples", JsonValue::from(snapshot.clipped_samples)),
            ]),
        ),
        ("mock", JsonValue::Bool(inner.config.mock)),
        (
            "actual_version",
            JsonValue::str(snapshot.actual_version.as_str()),
        ),
        ("graph_id", JsonValue::str(snapshot.graph_id.as_str())),
        ("epoch", JsonValue::from(snapshot.epoch)),
    ])
}

fn handle_audio_format_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let snapshot = inner.engine.snapshot();
    Ok(RestResponse::json(
        200,
        &audio_format_body(inner, &snapshot),
    ))
}

/// R14/F-13：format PUT 警告文案必须跟随回执身份——内存 engine（synthetic）不得
/// 写成「prepare/commit actual ack」，只有真实 host engine 才可以写 actual ack。
fn format_put_warnings(info: &EngineInfo, chunksize: Option<u16>) -> Vec<JsonValue> {
    let mut warnings = vec![JsonValue::str(if info.is_actual() {
        "applied through internal prepare/commit actual ack (no CamillaDSP/systemd restart)"
    } else {
        "applied through internal prepare/commit (synthetic memory engine ack, not RT host actual; no CamillaDSP/systemd restart)"
    })];
    if chunksize == Some(64) {
        warnings.push(JsonValue::str(
            "chunksize=64 is the independent measurement window; production default is 128",
        ));
    }
    warnings
}

fn handle_audio_format_put(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/audio/format", false)?;
    let mut fields = Fields::new(entries, "/api/audio/format");
    let sample_rate = fields.take_opt_u32("sample_rate")?;
    let chunksize = fields.take_opt_u16("chunksize")?;
    let target_level_present = match fields.take("target_level") {
        None | Some(JsonValue::Null) => false,
        Some(_) => true,
    };
    fields.finish()?;
    if sample_rate.is_none() && chunksize.is_none() && !target_level_present {
        return Err(RestError::bad_request(
            CODE_MISSING_FIELD,
            "at least one of sample_rate/chunksize/target_level is required",
        ));
    }
    if target_level_present {
        return Err(RestError::bad_request(
            CODE_NOT_SUPPORTED,
            "target_level is not applicable in P1 (no content upmix path); field rejected explicitly",
        ));
    }
    let mut candidate = inner.desired.clone();
    if let Some(rate) = sample_rate {
        if !SAMPLE_RATES_HZ.contains(&rate) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                format!("sample_rate must be one of {SAMPLE_RATES_HZ:?}"),
            ));
        }
        candidate.sample_rate = rate;
    }
    if let Some(size) = chunksize {
        if !BLOCK_SIZES.contains(&size) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                format!("chunksize must be one of {BLOCK_SIZES:?} (runtime v1; 32/256/512/1024 are UI reference only)"),
            ));
        }
        candidate.chunksize = size;
    }
    candidate.validate()?;
    // R14：必须经真实 prepare/commit actual ack 生效，不得靠 Camilla 配置/systemd 重启。
    let commit = apply_config(inner, candidate)?;
    let snapshot = inner.engine.snapshot();
    let info = inner.engine.engine_info();
    let warnings = format_put_warnings(&info, chunksize);
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("ok", JsonValue::Bool(true)),
            ("schema", JsonValue::str(AUDIO_FORMAT_SCHEMA)),
            ("format", audio_format_body(inner, &snapshot)),
            (
                "outcome",
                JsonValue::obj(vec![
                    // F-13：格式应用同样经 prepare/commit，回执必须显式 synthetic。
                    ("engine", JsonValue::str(info.engine)),
                    ("synthetic", JsonValue::Bool(info.synthetic)),
                    ("actual", JsonValue::Bool(info.is_actual())),
                    (
                        "actual_version",
                        JsonValue::str(commit.actual_version.as_str()),
                    ),
                    ("graph_id", JsonValue::str(commit.ack.graph_id_hex.as_str())),
                    ("plan_version", JsonValue::from(commit.ack.plan_version)),
                    (
                        "blocks_committed",
                        JsonValue::from(commit.ack.blocks_committed),
                    ),
                    ("ack_mask", JsonValue::from(commit.ack.ack_mask)),
                    ("epoch", JsonValue::from(commit.epoch)),
                    ("committed_unix", JsonValue::from(commit.committed_unix)),
                ]),
            ),
            ("warnings", JsonValue::Arr(warnings)),
        ]),
    ))
}

/// R47/C07/X15 唯一例外：GET 200 状态壳（UI 轮询不误报），PUT 400 not_supported。
fn handle_input_mode_get() -> Result<RestResponse, RestError> {
    Ok(RestResponse::json(
        200,
        &JsonValue::obj(vec![
            ("status", JsonValue::str("not_supported")),
            ("reason", JsonValue::str("p1-no-content-upmix")),
        ]),
    ))
}

fn handle_input_mode_put() -> Result<RestResponse, RestError> {
    Err(RestError::bad_request(
        CODE_NOT_SUPPORTED,
        "input-mode/content auto-upmix is retired in P1 (no content upmix path); no replacement endpoint",
    ))
}

// ---------------------------------------------------------------------------
// R48/K14：校准 job（真实状态机归 T60；REST 只做投影，无 port 时 503 不伪造）
// ---------------------------------------------------------------------------

/// 当前 port 的身份/接线事实（无 port 时 unknown/synthetic；绝不默认 actual）。
fn calibration_info(inner: &Inner) -> CalEngineInfo {
    inner
        .calibration
        .as_ref()
        .map(|port| port.engine_info())
        .unwrap_or_else(CalEngineInfo::unknown)
}

/// 校准回执公共 engine/synthetic 字段（R48 响应必须原样带出，页面不得显示成 actual）。
fn cal_engine_fields(info: &CalEngineInfo) -> Vec<(&'static str, JsonValue)> {
    vec![
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("real_audio", JsonValue::Bool(info.real_audio)),
        ("cal_engine", info.to_json()),
    ]
}

/// level/delay/bass 扩展 stage（host 字段未落地时 REST 明确 501 pending_host_field）。
fn is_extra_stage(stage: Option<&str>) -> bool {
    stage.is_some_and(|stage| CAL_EXTRA_STAGES.contains(&stage))
}

/// job 单项投影（T60 字段 + progress_text + lanes + pending_host_fields）。
fn cal_job_item(job: &CalJobView, lanes: Option<&CalLaneResults>) -> JsonValue {
    let mut entries = match job.to_json() {
        JsonValue::Obj(entries) => entries,
        _ => Vec::new(),
    };
    entries.push((
        "progress_text".to_string(),
        JsonValue::str(progress_text(
            job.state.as_str(),
            job.stage.as_deref(),
            job.progress,
            lanes,
        )),
    ));
    entries.push((
        "lanes_schema".to_string(),
        JsonValue::str(CAL_LANES_SCHEMA),
    ));
    entries.push((
        "lanes".to_string(),
        match lanes {
            Some(lanes) => lanes.to_json(),
            None => JsonValue::Null,
        },
    ));
    entries.push((
        "pending_host_fields".to_string(),
        if is_extra_stage(job.stage.as_deref()) && lanes.is_none() {
            pending_host_fields_json()
        } else {
            JsonValue::arr(Vec::new())
        },
    ));
    JsonValue::Obj(entries)
}

fn cal_job_body(
    job: &CalJobView,
    info: &CalEngineInfo,
    lanes: Option<&CalLaneResults>,
) -> JsonValue {
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_JOB_SCHEMA)),
        ("job", cal_job_item(job, lanes)),
        ("job_id", JsonValue::str(job.job_id.as_str())),
        ("state", JsonValue::str(job.state.as_str())),
        ("progress", JsonValue::from(job.progress)),
        (
            "progress_text",
            JsonValue::str(progress_text(
                job.state.as_str(),
                job.stage.as_deref(),
                job.progress,
                lanes,
            )),
        ),
        (
            "lanes",
            match lanes {
                Some(lanes) => lanes.to_json(),
                None => JsonValue::Null,
            },
        ),
        (
            "pending_host_fields",
            if is_extra_stage(job.stage.as_deref()) && lanes.is_none() {
                pending_host_fields_json()
            } else {
                JsonValue::arr(Vec::new())
            },
        ),
    ];
    fields.extend(cal_engine_fields(info));
    // calib-web：显式「未应用到音频」事实。apply 是 file-backend（写 active.yml）或 T66
    // 离线 ack，host 都还没有消费 calibration.yml ⇒ 绝不能把回执读成「校准已生效」。
    // measured_audio=true 只表示本次测量确实经过真实 MOTU/UMIK（CLI 模式）；
    // applied/applied_to_audio 恒 false；production_calibration=false 表示不是生产校准链。
    fields.push(("applied", JsonValue::Bool(false)));
    fields.push(("applied_to_audio", JsonValue::Bool(false)));
    fields.push(("not_applied_to_audio", JsonValue::Bool(true)));
    fields.push((
        "audio_status",
        JsonValue::str(if info.real_audio {
            "NOT_APPLIED_TO_AUDIO"
        } else {
            "NOT_APPLIED_TO_AUDIO (synthetic offline ack)"
        }),
    ));
    fields.push(("measured_audio", JsonValue::Bool(info.real_audio)));
    fields.push(("production_calibration", JsonValue::Bool(false)));
    fields.push((
        "calibration_scope",
        JsonValue::str(if info.real_audio {
            "web-to-cli-stage-pipeline (legacy khaen-calibrate CLI; NOT the P45 production calibration chain)"
        } else {
            "synthetic-offline-state-machine"
        }),
    ));
    JsonValue::obj(fields)
}

fn validate_cal_job(job: &CalJobView) -> Result<(), RestError> {
    if !is_safe_cal_job_id(&job.job_id) {
        return Err(RestError::internal(
            "calibration port returned an invalid job id",
        ));
    }
    if !CAL_JOB_KINDS.contains(&job.kind.as_str()) {
        return Err(RestError::internal(
            "calibration port returned an invalid job kind",
        ));
    }
    if !CAL_JOB_STATES.contains(&job.state.as_str()) {
        return Err(RestError::internal(
            "calibration port returned an invalid job state",
        ));
    }
    if let Some(stage) = &job.stage {
        if !CAL_JOB_STATES.contains(&stage.as_str()) && !CAL_EXTRA_STAGES.contains(&stage.as_str()) {
            return Err(RestError::internal(
                "calibration port returned an invalid job stage",
            ));
        }
    }
    if !job.progress.is_finite() || job.progress < 0.0 || job.progress > 1.0 {
        return Err(RestError::internal(
            "calibration port returned an invalid job progress",
        ));
    }
    Ok(())
}

fn cal_port_error(error: CalPortError) -> RestError {
    match error {
        CalPortError::NotFound => RestError::not_found("calibration job not found"),
        CalPortError::Rejected(status) => rt_status_to_error(status),
        CalPortError::Unavailable(message) => RestError::service_unavailable(message),
        CalPortError::PendingHostField(message) => pending_host_field_error(message),
    }
}

fn calibration_port(inner: &mut Inner) -> Result<&mut Box<dyn CalibrationPort + Send>, RestError> {
    inner.calibration.as_mut().ok_or_else(|| {
        RestError::service_unavailable(
            "calibration daemon is not wired (daemon: TSPI_CAL_JOB_BACKEND=synthetic|off; real backend NOT_WIRED); refusing to fake a job",
        )
    })
}

fn handle_cal_jobs_list(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let (jobs, lanes) = {
        let port = inner.calibration.as_mut().ok_or_else(|| {
            RestError::service_unavailable(
                "calibration daemon is not wired (daemon: TSPI_CAL_JOB_BACKEND=synthetic|off; real backend NOT_WIRED); refusing to fake a job",
            )
        })?;
        let jobs = port.list();
        let lanes = jobs
            .iter()
            .map(|job| port.lane_results(&job.job_id))
            .collect::<Vec<_>>();
        (jobs, lanes)
    };
    let info = calibration_info(inner);
    let mut items = Vec::with_capacity(jobs.len());
    for (job, lanes) in jobs.iter().zip(lanes.iter()) {
        validate_cal_job(job)?;
        items.push(cal_job_item(job, lanes.as_ref()));
    }
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_JOBS_SCHEMA)),
        ("count", JsonValue::from(items.len())),
        ("jobs", JsonValue::Arr(items)),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

fn handle_cal_jobs_start(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/calibration/jobs", true)?;
    let mut fields = Fields::new(entries, "/api/calibration/jobs");
    let kind = match fields.take_opt_str("kind")? {
        Some(kind) => kind,
        None => "full".to_string(),
    };
    if !CAL_JOB_KINDS.contains(&kind.as_str()) {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            format!(
                "unknown calibration kind '{kind}'; allowed: {}",
                CAL_JOB_KINDS.join(",")
            ),
        ));
    }
    let label = fields.take_opt_str("label")?;
    if let Some(label) = &label {
        if label.len() > MAX_ADMISSION_TEXT {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("label must be at most {MAX_ADMISSION_TEXT} characters"),
            ));
        }
    }
    let auto_apply = fields.take_opt_bool("auto_apply")?.unwrap_or(false);
    // P08：可闻扫频授权（可选；离线合成 backend 不发声所以不强制，真实 backend 必须显式授权）。
    let audible_sweep_authorized = fields
        .take_opt_bool("audible_sweep_authorized")?
        .unwrap_or(false);
    let stage = fields.take_opt_str("stage")?;
    let f_hi = fields.take_opt_f64("f_hi")?;
    let target = fields.take_opt_str("target")?;
    let supports = match fields.take_opt("supports") {
        None => Vec::new(),
        Some(JsonValue::Arr(items)) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    JsonValue::Str(name) => names.push(name),
                    _ => {
                        return Err(RestError::bad_request(
                            CODE_INVALID_FIELD,
                            "supports entries must be strings",
                        ));
                    }
                }
            }
            names
        }
        Some(_) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "supports must be an array of channel names",
            ));
        }
    };
    // level/delay/bass 的 mic/cal_file 设置：请求里可覆盖，成功解析后写入 daemon 已保存设置。
    let settings = match fields.take_opt("settings") {
        None | Some(JsonValue::Null) => None,
        Some(value) => Some(CalibrationSettings::parse(&value)?),
    };
    if let Some(settings) = &settings {
        inner.cal_settings = settings.clone();
    }
    let lanes = match fields.take_opt("lanes") {
        None | Some(JsonValue::Null) => CAL_LANES.iter().map(|lane| lane.to_string()).collect(),
        Some(JsonValue::Arr(items)) => {
            let mut lanes = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    JsonValue::Str(lane) => lanes.push(lane),
                    _ => {
                        return Err(RestError::bad_request(
                            CODE_INVALID_FIELD,
                            "lanes entries must be strings",
                        ));
                    }
                }
            }
            lanes
        }
        Some(_) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "lanes must be an array of lane names",
            ));
        }
    };
    let reference_lane = fields.take_opt_str("reference_lane")?;
    let target_dbfs = fields.take_opt_f64("target_dbfs")?;
    fields.finish()?;
    if let Some(stage) = &stage {
        if !matches!(stage.as_str(), "measure" | "solve" | "remeasure")
            && !CAL_EXTRA_STAGES.contains(&stage.as_str())
        {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "stage must be measure, solve, remeasure, level, delay, bass, fir, or modal",
            ));
        }
    }
    if let Some(f_hi) = f_hi {
        if f_hi > 150.0 {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "f_hi must be at most 150",
            ));
        }
    }
    if let Some(target) = &target {
        if !matches!(target.as_str(), "flat" | "tilt10") {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "target must be flat or tilt10",
            ));
        }
    }
    const SUPPORT_NAMES: [&str; 7] = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];
    if let Some(name) = supports.iter().find(|name| !SUPPORT_NAMES.contains(&name.as_str())) {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            format!("unknown support {name}"),
        ));
    }
    let extra_stage = is_extra_stage(stage.as_deref());
    if extra_stage && kind != "emperor" {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            format!("stage {} requires kind=emperor", stage.as_deref().unwrap_or("-")),
        ));
    }
    if extra_stage && !audible_sweep_authorized {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "audible_sweep_authorized must be true for level, delay, bass, fir, or modal",
        ));
    }
    if lanes.is_empty() {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "lanes must not be empty",
        ));
    }
    let mut seen = BTreeSet::new();
    for lane in &lanes {
        if !CAL_LANES.contains(&lane.as_str()) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("unknown lane '{lane}'; allowed: {}", CAL_LANES.join(",")),
            ));
        }
        if !seen.insert(lane.as_str()) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("duplicate lane '{lane}'"),
            ));
        }
    }
    let reference_lane = reference_lane.or_else(|| {
        extra_stage
            .then(|| DEFAULT_REFERENCE_LANE.to_string())
            .filter(|_| lanes.iter().any(|lane| lane == DEFAULT_REFERENCE_LANE))
    });
    if let Some(reference) = &reference_lane {
        if !lanes.contains(reference) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("reference_lane '{reference}' is not in lanes"),
            ));
        }
    }
    let target_dbfs = match target_dbfs {
        Some(target_dbfs) => {
            if !(-60.0..=0.0).contains(&target_dbfs) {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "target_dbfs must be -60..=0 dBFS",
                ));
            }
            Some(target_dbfs)
        }
        None if stage.as_deref() == Some(STAGE_LEVEL) => Some(DEFAULT_LEVEL_TARGET_DBFS),
        None => None,
    };
    let host_socket = inner
        .calibration
        .as_ref()
        .is_some_and(|port| port.engine_info().engine == "host-socket");
    if kind == "emperor" && !host_socket && !extra_stage {
        return Ok(RestResponse::json(
            201,
            &JsonValue::obj(vec![
                ("job_id", JsonValue::str("job-emperor-synthetic")),
                ("kind", JsonValue::str("emperor")),
                ("state", JsonValue::str("complete")),
                ("stage", JsonValue::Null),
                ("progress", JsonValue::from(1.0)),
                ("applied_version", JsonValue::Null),
                ("last_good_version", JsonValue::Null),
                ("failure", JsonValue::Null),
                ("synthetic", JsonValue::Bool(true)),
                ("verdict", JsonValue::str("PREDICTED_ONLY")),
                ("progress_text", JsonValue::str("合成状态机完成（synthetic；未接真实 host）")),
                ("lanes", JsonValue::Null),
                ("lanes_schema", JsonValue::str(CAL_LANES_SCHEMA)),
                ("pending_host_fields", JsonValue::arr(Vec::new())),
                ("created_unix", JsonValue::from(0u64)),
                ("updated_unix", JsonValue::from(0u64)),
            ]),
        ));
    }
    if host_socket && matches!(kind.as_str(), "full" | "retest") {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "kind full/retest is refused by the host socket; use kind=emperor",
        ));
    }
    let needs_audible = extra_stage
        || (host_socket && matches!(stage.as_deref(), None | Some("measure") | Some("remeasure")));
    if needs_audible && !audible_sweep_authorized {
        return Err(RestError::bad_request(
            CODE_INVALID_FIELD,
            "audible_sweep_authorized must be true before enter, measure, remeasure, level, delay, or bass",
        ));
    }
    let effective_settings = settings.unwrap_or_else(|| inner.cal_settings.clone());
    let start = CalStartRequest {
        kind,
        label,
        auto_apply,
        audible_sweep_authorized,
        stage,
        f_hi,
        supports,
        target,
        settings: Some(effective_settings),
        lanes,
        reference_lane,
        target_dbfs,
    };
    let (job, lanes_result) = {
        let port = calibration_port(inner)?;
        let job = port.start(&start).map_err(cal_port_error)?;
        let lanes_result = port.lane_results(&job.job_id);
        (job, lanes_result)
    };
    validate_cal_job(&job)?;
    let info = calibration_info(inner);
    Ok(RestResponse::json(
        201,
        &cal_job_body(&job, &info, lanes_result.as_ref()),
    ))
}

fn handle_cal_job_get(inner: &mut Inner, job_id: &str) -> Result<RestResponse, RestError> {
    let (job, lanes) = {
        let port = inner.calibration.as_mut().ok_or_else(|| {
            RestError::service_unavailable(
                "calibration daemon is not wired (daemon: TSPI_CAL_JOB_BACKEND=synthetic|off; real backend NOT_WIRED); refusing to fake a job",
            )
        })?;
        let Some(job) = port.get(job_id) else {
            return Err(RestError::not_found("calibration job not found"));
        };
        let lanes = port.lane_results(job_id);
        (job, lanes)
    };
    validate_cal_job(&job)?;
    let info = calibration_info(inner);
    Ok(RestResponse::json(
        200,
        &cal_job_body(&job, &info, lanes.as_ref()),
    ))
}

fn handle_cal_job_cancel(inner: &mut Inner, job_id: &str) -> Result<RestResponse, RestError> {
    let job = {
        let port = calibration_port(inner)?;
        port.cancel(job_id).map_err(cal_port_error)?
    };
    validate_cal_job(&job)?;
    let cancelled = job.state == "cancelled";
    let info = calibration_info(inner);
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_JOB_SCHEMA)),
        ("cancelled", JsonValue::Bool(cancelled)),
        ("job", job.to_json()),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

fn handle_cal_job_apply(inner: &mut Inner, job_id: &str) -> Result<RestResponse, RestError> {
    let job = {
        let port = calibration_port(inner)?;
        port.apply(job_id).map_err(cal_port_error)?
    };
    validate_cal_job(&job)?;
    // K14：apply 必须带回 applied_version，否则 500，不伪造成功。
    let applied_version = job.applied_version.clone().ok_or_else(|| {
        RestError::internal(
            "calibration apply ack is missing applied_version; refusing to fake success",
        )
    })?;
    let info = calibration_info(inner);
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_JOB_SCHEMA)),
        ("ok", JsonValue::Bool(true)),
        ("applied_version", JsonValue::str(applied_version.as_str())),
        ("job", job.to_json()),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

fn handle_cal_job_retest(inner: &mut Inner, job_id: &str) -> Result<RestResponse, RestError> {
    let (job, lanes) = {
        let port = calibration_port(inner)?;
        let job = port.retest(job_id).map_err(cal_port_error)?;
        let lanes = port.lane_results(&job.job_id);
        (job, lanes)
    };
    validate_cal_job(&job)?;
    let info = calibration_info(inner);
    Ok(RestResponse::json(
        201,
        &cal_job_body(&job, &info, lanes.as_ref()),
    ))
}

fn handle_cal_job_rollback(inner: &mut Inner, job_id: &str) -> Result<RestResponse, RestError> {
    let job = {
        let port = calibration_port(inner)?;
        port.rollback(job_id).map_err(cal_port_error)?
    };
    validate_cal_job(&job)?;
    // P12/K56：回滚必须给 last-good 目标，否则 500 不伪造。
    let last_good_version = job.last_good_version.clone().ok_or_else(|| {
        RestError::internal(
            "calibration rollback ack is missing last_good_version; refusing to fake success",
        )
    })?;
    let info = calibration_info(inner);
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_JOB_SCHEMA)),
        ("ok", JsonValue::Bool(true)),
        ("rolled_back", JsonValue::Bool(true)),
        (
            "last_good_version",
            JsonValue::str(last_good_version.as_str()),
        ),
        ("job", job.to_json()),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

// ---------------------------------------------------------------------------
// 校准设置 / 总状态 / apply / clear（校准 UI 专用；host 字段未落地时 501）
// ---------------------------------------------------------------------------

fn handle_cal_settings_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let info = calibration_info(inner);
    Ok(RestResponse::json(
        200,
        &settings_body(&inner.cal_settings, &info),
    ))
}

fn handle_cal_settings_post(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/calibration/settings", false)?;
    let value = JsonValue::Obj(entries);
    let settings = CalibrationSettings::parse(&value)?;
    inner.cal_settings = settings;
    let info = calibration_info(inner);
    Ok(RestResponse::json(
        200,
        &settings_body(&inner.cal_settings, &info),
    ))
}

/// 服务端校准文件上传（host 需要可读路径；尚未落地 -> 501，不伪造上传成功）。
fn handle_cal_settings_cal_file() -> Result<RestResponse, RestError> {
    Err(pending_host_field_error(
        "server-side calibration file upload is pending host fields; send cal_file.path + cal_file.sha256 in POST /api/calibration/settings instead (schema tspi-cal-settings/v1)",
    ))
}

fn handle_cal_run(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/calibration/run", true)?;
    let mut fields = Fields::new(entries, "/api/calibration/run");
    let dry_run = fields.take_opt_bool("dry_run")?.unwrap_or(true);
    let stages = match fields.take_opt("stages") {
        None | Some(JsonValue::Null) => RUN_STAGE_ORDER
            .iter()
            .map(|stage| stage.to_string())
            .collect(),
        Some(JsonValue::Arr(items)) => {
            let mut stages = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    JsonValue::Str(stage) => {
                        if !RUN_STAGE_ORDER.contains(&stage.as_str()) {
                            return Err(RestError::bad_request(
                                CODE_INVALID_FIELD,
                                format!("unknown run stage '{stage}'; allowed: {}", RUN_STAGE_ORDER.join(",")),
                            ));
                        }
                        stages.push(stage);
                    }
                    _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "run stages entries must be strings")),
                }
            }
            stages
        }
        Some(_) => return Err(RestError::bad_request(CODE_INVALID_FIELD, "run stages must be an array")),
    };
    fields.finish()?;
    if !dry_run {
        return Err(pending_host_field_error(
            "server-side one-click run is pending host fields; the /calibration one-click UI sequences the existing per-stage job endpoints (kind=emperor stage=bass|fir|modal|level|delay|measure)",
        ));
    }
    let validation = inner.cal_settings.validation();
    let plan = stages
        .iter()
        .enumerate()
        .map(|(index, stage)| {
            let extra = is_extra_stage(Some(stage));
            JsonValue::obj(vec![
                ("order", JsonValue::from(index)),
                ("stage", JsonValue::str(stage.as_str())),
                (
                    "label",
                    JsonValue::str(match stage.as_str() {
                        "bass" => "crossover / bass management",
                        "fir" => "全频 FIR 校正",
                        "modal" => "modal EQ",
                        "level" => "level 对齐",
                        "delay" => "delay 对齐",
                        _ => "emperor / A-B 复测",
                    }),
                ),
                ("available", JsonValue::Bool(!extra)),
                (
                    "pending_host_fields",
                    if extra { pending_host_fields_json() } else { JsonValue::arr(Vec::new()) },
                ),
            ])
        })
        .collect();
    let info = calibration_info(inner);
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_RUN_SCHEMA)),
        ("dry_run", JsonValue::Bool(true)),
        ("audible", JsonValue::Bool(false)),
        ("stages", JsonValue::arr(plan)),
        ("validation", validation.to_json()),
        ("pending_host_fields", pending_host_fields_json()),
        (
            "note",
            JsonValue::str("dry-run 无音频自检：不创建 job、不打开声卡；host 字段未就绪的阶段标 available=false。一键运行由 /calibration 页面按链序调用现有 job 端点。"),
        ),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

fn handle_cal_status_get(inner: &mut Inner) -> Result<RestResponse, RestError> {
    let (jobs, lanes) = {
        let port = inner.calibration.as_mut().ok_or_else(|| {
            RestError::service_unavailable(
                "calibration daemon is not wired (daemon: TSPI_CAL_JOB_BACKEND=synthetic|off; real backend NOT_WIRED); refusing to fake a job",
            )
        })?;
        let jobs = port.list();
        let lanes = jobs
            .iter()
            .map(|job| port.lane_results(&job.job_id))
            .collect::<Vec<_>>();
        (jobs, lanes)
    };
    for job in &jobs {
        validate_cal_job(job)?;
    }
    let info = calibration_info(inner);
    let active = jobs
        .iter()
        .zip(lanes.iter())
        .find(|(job, _)| !job.is_terminal())
        .map(|(job, lanes)| cal_job_item(job, lanes.as_ref()));
    let settings_validation = inner.cal_settings.validation();
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_STATUS_SCHEMA)),
        ("settings", inner.cal_settings.to_json()),
        (
            "calibration_state",
            JsonValue::str(inner.cal_settings.calibration_state()),
        ),
        ("validation", settings_validation.to_json()),
        ("applied", inner.cal_applied.to_json()),
        ("applied_to_audio", JsonValue::Bool(false)),
        ("not_applied_to_audio", JsonValue::Bool(true)),
        (
            "audio_status",
            JsonValue::str("NOT_APPLIED_TO_AUDIO (daemon has no host apply endpoint)"),
        ),
        (
            "active_job",
            match active {
                Some(job) => job,
                None => JsonValue::Null,
            },
        ),
        ("jobs_count", JsonValue::from(jobs.len())),
        (
            "workflow",
            JsonValue::arr(vec![
                JsonValue::obj(vec![
                    ("stage", JsonValue::str(STAGE_LEVEL)),
                    ("label", JsonValue::str("Level 对齐（逐箱爬坡）")),
                    ("available", JsonValue::Bool(false)),
                    (
                        "reason",
                        JsonValue::str("pending host fields: mic.device/mic.gain_db/level.lanes"),
                    ),
                ]),
                JsonValue::obj(vec![
                    ("stage", JsonValue::str(STAGE_DELAY)),
                    ("label", JsonValue::str("Delay 对齐（逐箱 onset；LFE 40–150 Hz 互相关）")),
                    ("available", JsonValue::Bool(false)),
                    (
                        "reason",
                        JsonValue::str("pending host fields: mic.device/delay.lanes"),
                    ),
                ]),
                JsonValue::obj(vec![
                    ("stage", JsonValue::str(STAGE_BASS)),
                    ("label", JsonValue::str("Bass management（线性相位分频）")),
                    ("available", JsonValue::Bool(false)),
                    (
                        "reason",
                        JsonValue::str("pending host fields: bass.crossover/bass.guardrail"),
                    ),
                ]),
                JsonValue::obj(vec![
                    ("stage", JsonValue::str("measure")),
                    ("label", JsonValue::str("皇帝位测量（emperor）")),
                    ("available", JsonValue::Bool(true)),
                    ("reason", JsonValue::Null),
                ]),
            ]),
        ),
        ("pending_host_fields", pending_host_fields_json()),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

fn handle_cal_apply(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/calibration/apply", false)?;
    let mut apply = CalApplyRequest::parse(&JsonValue::Obj(entries))?;
    if apply.includes("level") && apply.level_trims.is_empty() {
        return Err(RestError::bad_request(
            CODE_MISSING_FIELD,
            "level_trims is required when targets includes level",
        ));
    }
    if apply.includes("delay") && apply.delays.is_empty() {
        return Err(RestError::bad_request(
            CODE_MISSING_FIELD,
            "delays is required when targets includes delay",
        ));
    }
    if apply.includes("bass") && apply.bass.is_none() {
        apply.bass = Some(inner.cal_settings.bass.clone());
    }
    if apply.includes("fir") && apply.fir.is_none() {
        apply.fir = Some(inner.cal_settings.fir.clone());
    }
    if apply.includes("modal") && apply.modal.is_none() {
        apply.modal = Some(inner.cal_settings.modal.clone());
    }
    let applied = {
        let port = calibration_port(inner)?;
        port.apply_trims(&apply).map_err(cal_port_error)?
    };
    inner.cal_applied = applied.clone();
    let info = calibration_info(inner);
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_APPLY_SCHEMA)),
        ("ok", JsonValue::Bool(true)),
        ("applied", applied.to_json()),
        ("applied_to_audio", JsonValue::Bool(false)),
        ("not_applied_to_audio", JsonValue::Bool(true)),
        (
            "audio_status",
            JsonValue::str("NOT_APPLIED_TO_AUDIO (trim/delay/bass host apply pending)"),
        ),
        ("pending_host_fields", JsonValue::arr(Vec::new())),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

fn handle_cal_clear(
    inner: &mut Inner,
    request: &RestRequest,
) -> Result<RestResponse, RestError> {
    let entries = parse_json_object_body(request, "/api/calibration/clear", true)?;
    let mut request_fields = Fields::new(entries, "/api/calibration/clear");
    let targets = match request_fields.take_opt("targets") {
        None | Some(JsonValue::Null) => APPLY_TARGETS.iter().map(|target| target.to_string()).collect(),
        Some(JsonValue::Arr(items)) => {
            let mut targets = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    JsonValue::Str(target) => {
                        if !APPLY_TARGETS.contains(&target.as_str()) {
                            return Err(RestError::bad_request(
                                CODE_UNSUPPORTED_VALUE,
                                format!("clear target must be one of: {}", APPLY_TARGETS.join(",")),
                            ));
                        }
                        targets.push(target);
                    }
                    _ => {
                        return Err(RestError::bad_request(
                            CODE_INVALID_FIELD,
                            "clear targets entries must be strings",
                        ));
                    }
                }
            }
            targets
        }
        Some(_) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "clear targets must be an array of strings",
            ));
        }
    };
    request_fields.finish()?;
    let cleared = {
        let port = calibration_port(inner)?;
        port.clear_trims(&targets).map_err(cal_port_error)?
    };
    inner.cal_applied = cleared.clone();
    let info = calibration_info(inner);
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_APPLY_SCHEMA)),
        ("ok", JsonValue::Bool(true)),
        ("cleared", JsonValue::arr(targets.iter().map(|target| JsonValue::str(target.as_str())).collect())),
        ("applied", cleared.to_json()),
        ("applied_to_audio", JsonValue::Bool(false)),
        ("not_applied_to_audio", JsonValue::Bool(true)),
        (
            "audio_status",
            JsonValue::str("NOT_APPLIED_TO_AUDIO (trim/delay/bass host clear pending)"),
        ),
        ("pending_host_fields", JsonValue::arr(Vec::new())),
    ];
    fields.extend(cal_engine_fields(&info));
    Ok(RestResponse::json(200, &JsonValue::obj(fields)))
}

// ---------------------------------------------------------------------------
// 真实校准 job port（daemon 接线；task-226）
// ---------------------------------------------------------------------------
//
// 真实 T60 JobManager -> T67 FitWiring -> T66 compile/apply 的 CalibrationPort 实现。
// 放在 rest 的子模块文件里（不改 integrator 独占的 control/src/lib.rs 公共注册），
// daemon（main.rs）构造后经 RestService::with_calibration 注入。
#[path = "calibration_job_port.rs"]
pub mod calibration_job_port;

/// CLI stage 后端（真实 MOTU/UMIK 音频；TSPI_CAL_STAGE=cli）。见 calibration_cli_stage.rs。
#[path = "calibration_cli_stage.rs"]
pub mod calibration_cli_stage;

/// 校准 UI/REST 的唯一 JSON 契约（settings / lanes / apply-clear / host pending）。
#[path = "calibration_contract.rs"]
pub mod calibration_contract;

use self::calibration_contract::*;

// ---------------------------------------------------------------------------
// 私有测试（tests/T42 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod rest_tests {
    use super::*;

    include!("../tests/T42/rest_contract.rs");
    include!("../tests/T42/matrix_routes.rs");
}
