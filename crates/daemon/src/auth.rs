//! 认证与会话（M1 / T34，契约见 HANDOFF/20261126-khaen-webui-plan/PLAN.md §4.1/§4.2）。
//!
//! # 契约
//!
//! - GET  /api/auth/status -> {configured, authenticated}（匿名可访问）。
//! - POST /api/auth/setup {password, confirm} -> 201 {csrf, expires_in} + Set-Cookie；
//!   已配置 409；密码至少 8 字符、confirm 一致，否则 400 稳定错误码。
//! - POST /api/auth/login {password} -> 200 {csrf, expires_in} + Set-Cookie；
//!   未配置 409；密码错误 401；同一来源 60s 内失败 5 次后 429（带 Retry-After）。
//! - GET  /api/auth/session -> {authenticated, csrf, expires_at}；未登录 401。
//! - POST /api/auth/logout -> 清会话（幂等：没有会话也返回 200 并下发清 Cookie）。
//! - POST /api/auth/password {old, new} -> 校验旧密码后换新哈希；旧会话全部失效，返回新会话。
//!
//! # 存储与凭据
//!
//! - 密码只存 argon2id PHC 字符串，落在环境变量 KHAEN_STATE_DIR 指定的状态目录下
//!   （缺省 /var/lib/khaen），文件名 auth.json；Unix 上 0600，原子 rename 写入。
//!   明文密码与裸令牌从不写盘、不写日志。
//! - 会话 8h；Cookie 名 khaen_session，属性 HttpOnly; SameSite=Strict; Path=/
//!   （LAN HTTP，不加 Secure；Max-Age 与会话 TTL 一致）。
//! - CSRF 随机 32B 十六进制（64 字符），绑定会话；写方法（POST/PUT/PATCH/DELETE）走 Cookie
//!   会话时必须带 X-KHAEN-CSRF。
//! - X-KHAEN-Token 是 Cookie 的等价回退（值是会话 token）；用 token 认证时免 CSRF
//!   （token 不是浏览器会自动携带的凭据）。
//! - 会话持久化在同一个 auth.json（存 SHA-256(token)，不存裸 token），daemon 重启后
//!   已签发的 Cookie 仍可校验；改密码会清空全部旧会话。
//!
//! # 中间件
//!
//! 除 /healthz 与 /api/auth/status|setup|login 外，全部 /api 要求会话；失败返回
//! PLAN §4.1 的错误体 {"error":{"code","message","details"}}。所有 /api 响应追加
//! X-KHAEN-API-Version: 1、X-KHAEN-Web-Version、Cache-Control: no-store 头。
//!
//! # 错误码（稳定，客户端可依赖）
//!
//! 见 code 模块：unauthorized(401)、invalid_credentials(401)、forbidden(403, CSRF)、
//! already_configured/not_configured(409)、rate_limited(429)、
//! invalid_json/missing_field/invalid_field/weak_password/password_mismatch(400)、
//! internal_error(500)。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{CACHE_CONTROL, COOKIE, RETRY_AFTER, SET_COOKIE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use khaen_core::sha256::{hex_lower, sha256_hex};

use crate::{DaemonState, WEB_VERSION};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 会话 Cookie 名。
pub const SESSION_COOKIE: &str = "khaen_session";
/// 直接会话 token 头（Cookie 的等价回退；值 = 会话 token）。
pub const TOKEN_HEADER: &str = "X-KHAEN-Token";
/// 写方法 CSRF 头（Cookie 会话必填；token 认证免）。
pub const CSRF_HEADER: &str = "X-KHAEN-CSRF";
/// 响应头：API 版本（PLAN §4.1 要求 /api 响应带）。
pub const API_VERSION_HEADER: &str = "X-KHAEN-API-Version";
/// 响应头：控制台版本（构建期 webui/package.json 的 version）。
pub const WEB_VERSION_HEADER: &str = "X-KHAEN-Web-Version";
/// 状态目录环境变量：auth.json 落在该目录（缺省 /var/lib/khaen）。
pub const STATE_DIR_VAR: &str = "KHAEN_STATE_DIR";
/// 默认状态目录（生产）。
pub const DEFAULT_STATE_DIR: &str = "/var/lib/khaen";
/// 认证文件名（在状态目录下）。
pub const AUTH_FILE: &str = "auth.json";
/// 认证文件 schema 版本。
pub const STORE_VERSION: u64 = 1;
/// 会话 TTL：8h（PLAN §4.2）。
pub const SESSION_TTL_SECS: u64 = 8 * 60 * 60;
/// 密码最小长度（PLAN §4.2）。
pub const MIN_PASSWORD_LEN: usize = 8;
/// 会话 token 随机字节数（32B -> 64 位十六进制）。
pub const SESSION_TOKEN_BYTES: usize = 32;
/// CSRF 随机字节数（32B -> 64 位十六进制，PLAN §4.2）。
pub const CSRF_BYTES: usize = 32;
/// 并发会话上限（超出淘汰最旧；有界，防内存/文件无限增长）。
pub const MAX_SESSIONS: usize = 16;
/// 登录限速：窗口内最大失败次数（5 次/60s，PLAN §4.2）。
pub const MAX_FAILED_LOGINS: usize = 5;
/// 登录限速窗口（秒）。
pub const LOGIN_WINDOW_SECS: u64 = 60;
/// auth.json 在 Unix 上的权限位。
pub const AUTH_FILE_MODE: u32 = 0o600;

/// 稳定错误码（PLAN §4.1 的 error.code；测试与前端可依赖）。
pub mod code {
    /// 未认证（中间件 401）。
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// 认证失败（登录密码错误 / 改密码旧密码错误，401）。
    pub const INVALID_CREDENTIALS: &str = "invalid_credentials";
    /// 已认证但 CSRF 缺失/不匹配（403）。
    pub const FORBIDDEN: &str = "forbidden";
    /// 登录失败次数超限（429）。
    pub const RATE_LIMITED: &str = "rate_limited";
    /// 已配置密码后再次 setup（409）。
    pub const ALREADY_CONFIGURED: &str = "already_configured";
    /// 还没 setup 就登录（409）。
    pub const NOT_CONFIGURED: &str = "not_configured";
    /// 请求体不是合法 JSON（400）。
    pub const INVALID_JSON: &str = "invalid_json";
    /// 缺必填字段（400）。
    pub const MISSING_FIELD: &str = "missing_field";
    /// 字段类型/内容非法（400）。
    pub const INVALID_FIELD: &str = "invalid_field";
    /// 密码长度不足（400）。
    pub const WEAK_PASSWORD: &str = "weak_password";
    /// 两次密码不一致（400）。
    pub const PASSWORD_MISMATCH: &str = "password_mismatch";
    /// 内部错误（500；不泄露凭据）。
    pub const INTERNAL_ERROR: &str = "internal_error";
}

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 认证配置（可注入：测试用临时目录 + 低成本 KDF 参数，不依赖 /var/lib）。
///
/// 生产用 AuthConfig::from_env；测试用 AuthConfig::for_tests。
/// params 只影响新建哈希的代价（PHC 串自带参数，验证按串内参数走）。
#[derive(Debug, Clone)]
pub struct AuthConfig {
    state_dir: PathBuf,
    session_ttl_secs: u64,
    params: Params,
}

impl AuthConfig {
    /// 生产配置：状态目录来自 KHAEN_STATE_DIR（缺省 /var/lib/khaen），Argon2id 默认参数
    /// （m=19456 KiB、t=2、p=1，OWASP 推荐档），会话 8h。
    pub fn from_env() -> Self {
        let state_dir = std::env::var(STATE_DIR_VAR)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR));
        Self {
            state_dir,
            session_ttl_secs: SESSION_TTL_SECS,
            params: Params::default(),
        }
    }

    /// 指定状态目录（其余取 from_env 的默认）。
    pub fn with_state_dir(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            ..Self::from_env()
        }
    }

    /// 测试配置：临时状态目录 + 低成本 Argon2id 参数（debug 构建下也能秒级跑完）。
    ///
    /// 只给测试用；生产绝不能这样配（PHC 串会如实带上低成本参数，一眼可辨）。
    pub fn for_tests(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            session_ttl_secs: SESSION_TTL_SECS,
            params: Params::new(1024, 1, 1, None).expect("test argon2 params are valid"),
        }
    }

    /// 会话 TTL（秒；测试可用短 TTL 验过期）。
    pub fn with_session_ttl_secs(mut self, seconds: u64) -> Self {
        self.session_ttl_secs = seconds;
        self
    }

    /// 状态目录。
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// auth.json 的完整路径。
    pub fn auth_path(&self) -> PathBuf {
        self.state_dir.join(AUTH_FILE)
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 认证错误：HTTP 状态 + 稳定码 + PLAN §4.1 错误体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<u64>,
}

impl AuthError {
    /// 构造。
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    /// 401 未认证。
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            code::UNAUTHORIZED,
            "authentication required: log in or present a valid X-KHAEN-Token",
        )
    }

    /// 401 凭据错误。
    pub fn invalid_credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            code::INVALID_CREDENTIALS,
            "invalid password",
        )
    }

    /// 403 CSRF 失败。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code::FORBIDDEN, message)
    }

    /// 400。
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// 409。
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// 429：带 Retry-After 秒数。
    pub fn rate_limited(retry_after_secs: u64) -> Self {
        let mut error = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            code::RATE_LIMITED,
            format!("too many failed login attempts; retry after {retry_after_secs}s"),
        );
        error.retry_after = Some(retry_after_secs);
        error
    }

    /// 500（不泄露凭据；message 可带文件名但不带密码/令牌）。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            code::INTERNAL_ERROR,
            message,
        )
    }

    /// HTTP 状态。
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人可读说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.code,
            self.status.as_u16(),
            self.message
        )
    }
}

impl std::error::Error for AuthError {}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "details": {},
            }
        });
        let mut response = (self.status, Json(body)).into_response();
        if let Some(seconds) = self.retry_after {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
        }
        response
    }
}

// ---------------------------------------------------------------------------
// 会话数据
// ---------------------------------------------------------------------------

/// 认证方式：Cookie 会话（写方法要 CSRF）或直接 token（免 CSRF）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// 浏览器 Cookie 会话。
    Cookie,
    /// X-KHAEN-Token 直接会话 token。
    Token,
}

/// 一次请求解析出的会话视图。
#[derive(Debug, Clone)]
pub struct SessionView {
    /// 会话 token（Cookie 值 / X-KHAEN-Token 值）。
    pub token: String,
    /// 绑定会话的 CSRF（32B 十六进制）。
    pub csrf: String,
    /// 到期时刻（Unix 秒）。
    pub expires_at: u64,
    /// 本次请求的认证方式。
    pub kind: AuthKind,
}

/// 新签发会话的结果。
#[derive(Debug, Clone)]
pub(crate) struct Issued {
    token: String,
    csrf: String,
    expires_in: u64,
}

/// 持久化的会话记录（只存 token 的 SHA-256，不存裸 token）。
#[derive(Debug, Clone)]
struct StoredSession {
    csrf: String,
    created: u64,
    expires: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn random_bytes(len: usize) -> Result<Vec<u8>, AuthError> {
    let mut buffer = vec![0u8; len];
    getrandom::getrandom(&mut buffer)
        .map_err(|error| AuthError::internal(format!("OS random source failed: {error}")))?;
    Ok(buffer)
}

fn random_hex(len: usize) -> Result<String, AuthError> {
    Ok(hex_lower(&random_bytes(len)?))
}

fn is_hex(len: usize, value: &str) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn session_key(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (lhs, rhs) in left.iter().zip(right.iter()) {
        difference |= lhs ^ rhs;
    }
    difference == 0
}

/// Unix 秒 -> RFC3339 UTC（例如 2026-11-26T05:03:55Z）。
///
/// 只做 UTC（协议要求时间戳 RFC3339 UTC）；不引入时区依赖。
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let seconds = (unix_secs % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Howard Hinnant 的 civil_from_days（Unix 纪元起的天数 -> 公历日期）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

/// 认证存储：密码 PHC、会话、登录失败计数。
///
/// 所有方法都是短的同步临界区；由 DaemonState 的 Mutex 保护（非 RT 路径）。
#[derive(Debug)]
pub(crate) struct AuthStore {
    config: AuthConfig,
    password: Option<String>,
    sessions: BTreeMap<String, StoredSession>,
    failures: HashMap<String, VecDeque<u64>>,
}

impl AuthStore {
    /// 从配置的 auth.json 加载；文件不存在 = 未配置（走 setup 流程）。
    ///
    /// 文件存在但内容坏掉时 fail-closed：返回内部错误（daemon 启动失败），不静默当成未配置，
    /// 避免把已有密码覆盖掉。
    pub(crate) fn load(config: AuthConfig) -> Result<Self, AuthError> {
        let path = config.auth_path();
        let (password, sessions) = match std::fs::read(&path) {
            Ok(bytes) => {
                let parsed = parse_store(&bytes)?;
                let _ = restrict_file_permissions(&path);
                parsed
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, BTreeMap::new()),
            Err(error) => {
                return Err(AuthError::internal(format!(
                    "cannot read auth store {}: {error}",
                    path.display()
                )))
            }
        };
        let now = now_unix();
        let sessions = sessions
            .into_iter()
            .filter(|(_, session)| session.expires > now)
            .collect();
        Ok(Self {
            config,
            password,
            sessions,
            failures: HashMap::new(),
        })
    }

    /// 是否已设置密码（setup 完成）。
    pub(crate) fn configured(&self) -> bool {
        self.password.is_some()
    }

    /// setup：仅未配置时可用；返回新会话。
    pub(crate) fn setup(&mut self, password: &str, confirm: &str) -> Result<Issued, AuthError> {
        if self.configured() {
            return Err(AuthError::conflict(
                code::ALREADY_CONFIGURED,
                "admin password is already configured; use /api/auth/login",
            ));
        }
        validate_new_password(password)?;
        if password != confirm {
            return Err(AuthError::bad_request(
                code::PASSWORD_MISMATCH,
                "password and confirm do not match",
            ));
        }
        self.password = Some(self.hash_new_password(password)?);
        // setup 之前不应有会话；清空保证“未配置 -> 已配置”只留新会话。
        self.sessions.clear();
        let issued = self.new_session(now_unix())?;
        self.persist()?;
        Ok(issued)
    }

    /// 登录：校验密码并签发新会话。
    pub(crate) fn login(&mut self, password: &str) -> Result<Issued, AuthError> {
        let Some(stored) = self.password.clone() else {
            return Err(AuthError::conflict(
                code::NOT_CONFIGURED,
                "admin password is not configured; use /api/auth/setup",
            ));
        };
        if !self.verify_password(password, &stored)? {
            return Err(AuthError::invalid_credentials());
        }
        let issued = self.new_session(now_unix())?;
        self.persist()?;
        Ok(issued)
    }

    /// 解析已签发会话（过期即作废）。
    pub(crate) fn resolve(&mut self, token: &str) -> Option<SessionView> {
        let key = session_key(token);
        let now = now_unix();
        let session = self.sessions.get(&key)?;
        if session.expires <= now {
            self.sessions.remove(&key);
            return None;
        }
        Some(SessionView {
            token: token.to_string(),
            csrf: session.csrf.clone(),
            expires_at: session.expires,
            kind: AuthKind::Cookie,
        })
    }

    /// 注销会话（不存在也算成功 —— 幂等）。
    pub(crate) fn logout(&mut self, token: &str) -> Result<(), AuthError> {
        let removed = self.sessions.remove(&session_key(token)).is_some();
        if removed {
            self.persist()?;
        }
        Ok(())
    }

    /// 改密码：旧密码校验通过后换哈希、作废全部旧会话、签发新会话。
    pub(crate) fn change_password(
        &mut self,
        old_password: &str,
        new_password: &str,
    ) -> Result<Issued, AuthError> {
        let Some(stored) = self.password.clone() else {
            return Err(AuthError::conflict(
                code::NOT_CONFIGURED,
                "admin password is not configured; use /api/auth/setup",
            ));
        };
        if !self.verify_password(old_password, &stored)? {
            return Err(AuthError::invalid_credentials());
        }
        validate_new_password(new_password)?;
        let credential = self.hash_new_password(new_password)?;
        self.password = Some(credential);
        // 旧会话全部失效；随后只签发一个新会话（调用方拿到新 Cookie）。
        self.sessions.clear();
        let issued = self.new_session(now_unix())?;
        self.persist()?;
        Ok(issued)
    }

    /// 登录限速：返回需要等待的秒数（未限速返回 None）。
    pub(crate) fn login_retry_after(&mut self, source: &str) -> Option<u64> {
        let now = now_unix();
        self.prune_failures(now);
        let failures = self.failures.get(source)?;
        if failures.len() < MAX_FAILED_LOGINS {
            return None;
        }
        let oldest = *failures.front()?;
        let retry = (oldest + LOGIN_WINDOW_SECS).saturating_sub(now).max(1);
        Some(retry)
    }

    /// 记录一次登录失败（同一来源）。
    pub(crate) fn record_login_failure(&mut self, source: &str) {
        let now = now_unix();
        self.prune_failures(now);
        self.failures
            .entry(source.to_string())
            .or_default()
            .push_back(now);
    }

    /// 登录成功后清空该来源的失败计数。
    pub(crate) fn clear_login_failures(&mut self, source: &str) {
        self.failures.remove(source);
    }

    /// 过期会话清理。
    fn purge_expired(&mut self, now: u64) {
        self.sessions.retain(|_, session| session.expires > now);
    }

    fn prune_failures(&mut self, now: u64) {
        let window = LOGIN_WINDOW_SECS;
        for failures in self.failures.values_mut() {
            while let Some(&front) = failures.front() {
                if front.saturating_add(window) <= now {
                    failures.pop_front();
                } else {
                    break;
                }
            }
        }
        self.failures.retain(|_, failures| !failures.is_empty());
    }

    /// 签发新会话（有界：超过 MAX_SESSIONS 淘汰最旧）。
    fn new_session(&mut self, now: u64) -> Result<Issued, AuthError> {
        self.purge_expired(now);
        while self.sessions.len() >= MAX_SESSIONS {
            let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.created)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.sessions.remove(&oldest);
        }
        let token = random_hex(SESSION_TOKEN_BYTES)?;
        let csrf = random_hex(CSRF_BYTES)?;
        let ttl = self.config.session_ttl_secs;
        self.sessions.insert(
            session_key(&token),
            StoredSession {
                csrf: csrf.clone(),
                created: now,
                expires: now.saturating_add(ttl),
            },
        );
        Ok(Issued {
            token,
            csrf,
            expires_in: ttl,
        })
    }

    fn hash_new_password(&self, password: &str) -> Result<String, AuthError> {
        let salt_bytes = random_bytes(16)?;
        let salt = SaltString::encode_b64(&salt_bytes).map_err(|error| {
            AuthError::internal(format!("cannot encode password salt: {error}"))
        })?;
        let argon = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            self.config.params.clone(),
        );
        argon
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|error| AuthError::internal(format!("cannot hash password: {error}")))
    }

    fn verify_password(&self, password: &str, stored: &str) -> Result<bool, AuthError> {
        let parsed = PasswordHash::new(stored).map_err(|error| {
            AuthError::internal(format!("stored password hash is invalid: {error}"))
        })?;
        // 验证按 PHC 串里的参数执行（password-hash 的默认实现），与进程配置无关。
        let argon = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            self.config.params.clone(),
        );
        Ok(argon.verify_password(password.as_bytes(), &parsed).is_ok())
    }

    fn to_json(&self) -> Value {
        let sessions: Vec<Value> = self
            .sessions
            .iter()
            .map(|(key, session)| {
                json!({
                    "token_sha256": key,
                    "csrf": session.csrf,
                    "created": session.created,
                    "expires": session.expires,
                })
            })
            .collect();
        json!({
            "version": STORE_VERSION,
            "password": self.password,
            "sessions": sessions,
        })
    }

    /// 原子写 auth.json（Unix 0600 + rename；Windows 直接覆盖）。
    fn persist(&self) -> Result<(), AuthError> {
        let directory = self.config.state_dir();
        std::fs::create_dir_all(directory).map_err(|error| {
            AuthError::internal(format!(
                "cannot create auth directory {}: {error}",
                directory.display()
            ))
        })?;
        let _ = restrict_directory_permissions(directory);

        let mut bytes = serde_json::to_vec_pretty(&self.to_json())
            .map_err(|error| AuthError::internal(format!("cannot encode auth store: {error}")))?;
        bytes.push(b'\n');

        let path = self.config.auth_path();
        let temporary = path.with_extension("json.tmp");
        write_private_file(&temporary, &bytes).map_err(|error| {
            AuthError::internal(format!(
                "cannot write auth store {}: {error}",
                temporary.display()
            ))
        })?;
        replace_file(&temporary, &path).map_err(|error| {
            AuthError::internal(format!(
                "cannot replace auth store {}: {error}",
                path.display()
            ))
        })?;
        Ok(())
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(AUTH_FILE_MODE);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // Unix 上 rename 原子覆盖。
        std::fs::rename(temporary, destination)
    }
    #[cfg(not(unix))]
    {
        // Windows 的 rename 不覆盖已存在文件：先删再改名（单写者 + 小文件，窗口可接受）。
        if destination.exists() {
            std::fs::remove_file(destination)?;
        }
        std::fs::rename(temporary, destination)
    }
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(AUTH_FILE_MODE))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> std::io::Result<()> {
    // Windows 没有 POSIX 权限位；ACL 由部署层负责。
    Ok(())
}

fn restrict_directory_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// 解析 auth.json：{version, password, sessions:[{token_sha256, csrf, created, expires}]}。
fn parse_store(
    bytes: &[u8],
) -> Result<(Option<String>, BTreeMap<String, StoredSession>), AuthError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| AuthError::internal(format!("auth store is not valid JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| AuthError::internal("auth store must be a JSON object"))?;
    let version = object.get("version").and_then(Value::as_u64);
    if version != Some(STORE_VERSION) {
        return Err(AuthError::internal(format!(
            "unsupported auth store version {version:?} (expected {STORE_VERSION})"
        )));
    }
    let password = match object.get("password") {
        None | Some(Value::Null) => None,
        Some(Value::String(phc)) => {
            if !phc.starts_with("$argon2id$") {
                return Err(AuthError::internal(
                    "auth store password is not an argon2id PHC string",
                ));
            }
            Some(phc.clone())
        }
        Some(_) => {
            return Err(AuthError::internal(
                "auth store password must be an argon2id PHC string or null",
            ))
        }
    };
    let entries = object
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or_else(|| AuthError::internal("auth store must contain a sessions array"))?;
    let mut sessions = BTreeMap::new();
    for entry in entries {
        let entry = entry
            .as_object()
            .ok_or_else(|| AuthError::internal("auth store session must be an object"))?;
        let key = entry
            .get("token_sha256")
            .and_then(Value::as_str)
            .filter(|value| is_hex(64, value))
            .ok_or_else(|| {
                AuthError::internal("auth store session token_sha256 must be 64 hex chars")
            })?;
        let csrf = entry
            .get("csrf")
            .and_then(Value::as_str)
            .filter(|value| is_hex(CSRF_BYTES * 2, value))
            .ok_or_else(|| AuthError::internal("auth store session csrf must be 64 hex chars"))?;
        let created = entry
            .get("created")
            .and_then(Value::as_u64)
            .ok_or_else(|| AuthError::internal("auth store session created must be an integer"))?;
        let expires = entry
            .get("expires")
            .and_then(Value::as_u64)
            .ok_or_else(|| AuthError::internal("auth store session expires must be an integer"))?;
        sessions.insert(
            key.to_string(),
            StoredSession {
                csrf: csrf.to_string(),
                created,
                expires,
            },
        );
    }
    Ok((password, sessions))
}

// ---------------------------------------------------------------------------
// HTTP 层：中间件
// ---------------------------------------------------------------------------

/// 认证中间件（挂在整棵路由上；只对 /api 生效）。
pub(crate) async fn guard(
    State(state): State<Arc<DaemonState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if !crate::webui::is_api_path(&path) {
        return next.run(request).await;
    }
    let method = request.method().clone();
    let source = source_key(&request);
    let is_login = path == "/api/auth/login" && method == Method::POST;

    // 登录限速：限速期内一律 429（含正确密码），Retry-After 提示窗口剩余秒数。
    if is_login {
        if let Some(seconds) = state.auth().login_retry_after(&source) {
            return decorate(AuthError::rate_limited(seconds).into_response());
        }
    }

    if is_public_path(&path) {
        let response = next.run(request).await;
        if is_login {
            let mut auth = state.auth();
            match response.status() {
                StatusCode::OK => auth.clear_login_failures(&source),
                StatusCode::UNAUTHORIZED => auth.record_login_failure(&source),
                _ => {}
            }
        }
        return decorate(response);
    }

    let session = resolve_session(&state, request.headers());
    let Some(session) = session else {
        // logout 幂等：没有有效会话也返回 200 + 清 Cookie（不是 401）。
        if path == "/api/auth/logout" && method == Method::POST {
            return decorate(logout_response());
        }
        return decorate(AuthError::unauthorized().into_response());
    };

    // 写方法额外要求 CSRF：Cookie 会话必填，token 认证免（token 不是浏览器自动携带的凭据）。
    if session.kind == AuthKind::Cookie && is_write_method(&method) {
        let csrf = request
            .headers()
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .unwrap_or_default();
        if csrf.is_empty() || !constant_time_eq(csrf.as_bytes(), session.csrf.as_bytes()) {
            return decorate(
                AuthError::forbidden(
                    "missing or invalid CSRF token: Cookie sessions must send X-KHAEN-CSRF",
                )
                .into_response(),
            );
        }
    }

    decorate(next.run(request).await)
}

/// /api 公开路径（/healthz 本来就不在 /api 下）。
pub(crate) fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        "/api/auth/status" | "/api/auth/setup" | "/api/auth/login"
    )
}

/// 写方法（要求 CSRF 的方法集合）。
pub(crate) fn is_write_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn source_key(request: &Request) -> String {
    request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|address| address.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 给 /api 响应补 PLAN §4.1 要求的响应头。
fn decorate(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("x-khaen-api-version"),
        HeaderValue::from_static("1"),
    );
    headers.insert(
        HeaderName::from_static("x-khaen-web-version"),
        HeaderValue::from_static(WEB_VERSION),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// ---------------------------------------------------------------------------
// HTTP 层：handler
// ---------------------------------------------------------------------------

/// GET /api/auth/status（匿名可访问）：{configured, authenticated}。
pub(crate) async fn status(State(state): State<Arc<DaemonState>>, headers: HeaderMap) -> Response {
    let configured = state.auth().configured();
    let authenticated = resolve_session(&state, &headers).is_some();
    (
        StatusCode::OK,
        Json(json!({
            "configured": configured,
            "authenticated": authenticated,
        })),
    )
        .into_response()
}

/// POST /api/auth/setup（匿名可访问；已配置 409）。
pub(crate) async fn setup(State(state): State<Arc<DaemonState>>, body: Bytes) -> Response {
    let payload = match parse_object(&body) {
        Ok(payload) => payload,
        Err(error) => return error.into_response(),
    };
    let password = match string_field(&payload, "password") {
        Ok(value) => value.to_string(),
        Err(error) => return error.into_response(),
    };
    let confirm = match string_field(&payload, "confirm") {
        Ok(value) => value.to_string(),
        Err(error) => return error.into_response(),
    };
    let issued = match state.auth().setup(&password, &confirm) {
        Ok(issued) => issued,
        Err(error) => return error.into_response(),
    };
    session_response(StatusCode::CREATED, &issued)
}

/// POST /api/auth/login（匿名可访问）。
pub(crate) async fn login(State(state): State<Arc<DaemonState>>, body: Bytes) -> Response {
    let payload = match parse_object(&body) {
        Ok(payload) => payload,
        Err(error) => return error.into_response(),
    };
    let password = match string_field(&payload, "password") {
        Ok(value) => value.to_string(),
        Err(error) => return error.into_response(),
    };
    let issued = match state.auth().login(&password) {
        Ok(issued) => issued,
        Err(error) => return error.into_response(),
    };
    session_response(StatusCode::OK, &issued)
}

/// GET /api/auth/session（要求会话）：{authenticated, csrf, expires_at}。
pub(crate) async fn session(State(state): State<Arc<DaemonState>>, headers: HeaderMap) -> Response {
    let Some(view) = resolve_session(&state, &headers) else {
        return AuthError::unauthorized().into_response();
    };
    (
        StatusCode::OK,
        Json(json!({
            "authenticated": true,
            "csrf": view.csrf,
            "expires_at": rfc3339_utc(view.expires_at),
        })),
    )
        .into_response()
}

/// POST /api/auth/logout（幂等）：清会话 + 清 Cookie。
pub(crate) async fn logout(State(state): State<Arc<DaemonState>>, headers: HeaderMap) -> Response {
    if let Some(view) = resolve_session(&state, &headers) {
        if let Err(error) = state.auth().logout(&view.token) {
            return error.into_response();
        }
    }
    logout_response()
}

/// POST /api/auth/password（要求登录 + CSRF）：{old, new}。
pub(crate) async fn password(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if resolve_session(&state, &headers).is_none() {
        return AuthError::unauthorized().into_response();
    }
    let payload = match parse_object(&body) {
        Ok(payload) => payload,
        Err(error) => return error.into_response(),
    };
    let old_password = match string_field(&payload, "old") {
        Ok(value) => value.to_string(),
        Err(error) => return error.into_response(),
    };
    let new_password = match string_field(&payload, "new") {
        Ok(value) => value.to_string(),
        Err(error) => return error.into_response(),
    };
    let issued = match state.auth().change_password(&old_password, &new_password) {
        Ok(issued) => issued,
        Err(error) => return error.into_response(),
    };
    session_response(StatusCode::OK, &issued)
}

// ---------------------------------------------------------------------------
// HTTP 层：小工具
// ---------------------------------------------------------------------------

fn parse_object(body: &[u8]) -> Result<Map<String, Value>, AuthError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| {
        AuthError::bad_request(
            code::INVALID_JSON,
            format!("request body must be valid JSON: {error}"),
        )
    })?;
    value.as_object().cloned().ok_or_else(|| {
        AuthError::bad_request(code::INVALID_JSON, "request body must be a JSON object")
    })
}

fn string_field<'a>(payload: &'a Map<String, Value>, name: &str) -> Result<&'a str, AuthError> {
    match payload.get(name) {
        Some(Value::String(value)) => Ok(value.as_str()),
        Some(_) => Err(AuthError::bad_request(
            code::INVALID_FIELD,
            format!("field {name} must be a string"),
        )),
        None => Err(AuthError::bad_request(
            code::MISSING_FIELD,
            format!("missing field: {name}"),
        )),
    }
}

fn validate_new_password(password: &str) -> Result<(), AuthError> {
    let length = password.chars().count();
    if length < MIN_PASSWORD_LEN {
        return Err(AuthError::bad_request(
            code::WEAK_PASSWORD,
            format!("password must be at least {MIN_PASSWORD_LEN} characters (got {length})"),
        ));
    }
    Ok(())
}

fn session_cookie(token: &str, max_age_secs: u64) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}")
}

fn clear_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

pub(crate) fn session_response(status: StatusCode, issued: &Issued) -> Response {
    let body = json!({
        "csrf": issued.csrf,
        "expires_in": issued.expires_in,
    });
    let mut response = (status, Json(body)).into_response();
    if let Ok(cookie) = HeaderValue::from_str(&session_cookie(&issued.token, issued.expires_in)) {
        response.headers_mut().insert(SET_COOKIE, cookie);
    }
    response
}

fn logout_response() -> Response {
    let mut response = (StatusCode::OK, Json(json!({ "ok": true }))).into_response();
    if let Ok(cookie) = HeaderValue::from_str(&clear_session_cookie()) {
        response.headers_mut().insert(SET_COOKIE, cookie);
    }
    response
}

/// 从请求头解析本次请求的凭据：X-KHAEN-Token 优先，其次 khaen_session Cookie。
fn presented_token(headers: &HeaderMap) -> Option<(String, AuthKind)> {
    if let Some(value) = headers.get(TOKEN_HEADER) {
        if let Ok(raw) = value.to_str() {
            let token = raw.trim();
            if !token.is_empty() {
                return Some((token.to_string(), AuthKind::Token));
            }
        }
    }
    let cookies = headers.get(COOKIE)?.to_str().ok()?;
    for part in cookies.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if name.trim() == SESSION_COOKIE {
            let token = value.trim();
            if !token.is_empty() {
                return Some((token.to_string(), AuthKind::Cookie));
            }
        }
    }
    None
}

/// 解析并校验会话（过期会话返回 None，同时从存储里清掉）。
fn resolve_session(state: &DaemonState, headers: &HeaderMap) -> Option<SessionView> {
    let (token, kind) = presented_token(headers)?;
    let mut view = state.auth().resolve(&token)?;
    view.kind = kind;
    Some(view)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "khaen-auth-unit-{}-{tag}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }

        fn config(&self) -> AuthConfig {
            AuthConfig::for_tests(&self.0)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const PASSWORD: &str = "correct horse battery";

    fn store(dir: &TempDir) -> AuthStore {
        AuthStore::load(dir.config()).expect("load auth store")
    }

    #[test]
    fn setup_then_login_issues_sessions_and_rejects_wrong_password() {
        let dir = TempDir::new("login");
        let mut auth = store(&dir);
        assert!(!auth.configured());

        let issued = auth.setup(PASSWORD, PASSWORD).expect("setup");
        assert!(auth.configured());
        assert_eq!(issued.expires_in, SESSION_TTL_SECS);
        assert_eq!(issued.token.len(), SESSION_TOKEN_BYTES * 2);
        assert_eq!(issued.csrf.len(), CSRF_BYTES * 2);
        let view = auth.resolve(&issued.token).expect("session resolves");
        assert_eq!(view.csrf, issued.csrf);

        // 错误密码：401 invalid_credentials，不签发会话。
        let error = auth.login("wrong password").expect_err("wrong password");
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(error.code(), code::INVALID_CREDENTIALS);

        // 正确密码：新会话可用。
        let logged_in = auth.login(PASSWORD).expect("login");
        assert_ne!(logged_in.token, issued.token);
        assert!(auth.resolve(&logged_in.token).is_some());

        // 注销幂等：第一次移除，第二次也成功。
        auth.logout(&logged_in.token).expect("logout");
        assert!(auth.resolve(&logged_in.token).is_none());
        auth.logout(&logged_in.token).expect("logout is idempotent");
    }

    #[test]
    fn setup_is_one_shot_and_validates_input() {
        let dir = TempDir::new("setup");
        let mut auth = store(&dir);

        let weak = auth.setup("short", "short").expect_err("short password");
        assert_eq!(weak.status(), StatusCode::BAD_REQUEST);
        assert_eq!(weak.code(), code::WEAK_PASSWORD);

        let mismatch = auth
            .setup(PASSWORD, "different password")
            .expect_err("mismatch");
        assert_eq!(mismatch.code(), code::PASSWORD_MISMATCH);
        assert!(!auth.configured(), "失败的 setup 不能把状态改成已配置");

        auth.setup(PASSWORD, PASSWORD).expect("setup");
        let again = auth
            .setup(PASSWORD, PASSWORD)
            .expect_err("already configured");
        assert_eq!(again.status(), StatusCode::CONFLICT);
        assert_eq!(again.code(), code::ALREADY_CONFIGURED);
    }

    #[test]
    fn login_before_setup_is_rejected() {
        let dir = TempDir::new("not-configured");
        let mut auth = store(&dir);
        let error = auth.login(PASSWORD).expect_err("not configured");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), code::NOT_CONFIGURED);
    }

    #[test]
    fn rate_limiter_blocks_after_five_failures_in_window() {
        let dir = TempDir::new("rate");
        let mut auth = store(&dir);
        let source = "192.0.2.10";
        for _ in 0..MAX_FAILED_LOGINS {
            assert_eq!(auth.login_retry_after(source), None);
            auth.record_login_failure(source);
        }
        let retry = auth.login_retry_after(source).expect("rate limited");
        assert!((1..=LOGIN_WINDOW_SECS).contains(&retry), "{retry}");
        // 其他来源不受影响（限速按来源隔离）。
        assert_eq!(auth.login_retry_after("192.0.2.11"), None);
        // 成功登录清空计数。
        auth.clear_login_failures(source);
        assert_eq!(auth.login_retry_after(source), None);
    }

    #[test]
    fn state_survives_reload_and_password_change_invalidates_sessions() {
        let dir = TempDir::new("restart");
        let mut auth = store(&dir);
        let first = auth.setup(PASSWORD, PASSWORD).expect("setup");
        assert_eq!(auth.config.session_ttl_secs, SESSION_TTL_SECS);
        drop(auth);

        let mut reloaded = store(&dir);
        assert!(reloaded.configured(), "重启后配置从 auth.json 恢复");
        assert!(reloaded.resolve(&first.token).is_some());
        assert!(reloaded.login(PASSWORD).is_ok());
        assert_eq!(
            reloaded.login("wrong").expect_err("wrong password").code(),
            code::INVALID_CREDENTIALS
        );

        let second = reloaded.login(PASSWORD).expect("login");
        let issued = reloaded
            .change_password(PASSWORD, "a new long password")
            .expect("change password");
        assert!(reloaded.resolve(&second.token).is_none(), "旧会话必须失效");
        assert!(reloaded.resolve(&issued.token).is_some(), "新会话必须可用");
        drop(reloaded);

        // 新密码在重启后仍有效，旧密码失效。
        let mut reloaded = store(&dir);
        assert!(reloaded.login("a new long password").is_ok());
        assert_eq!(
            reloaded.login(PASSWORD).expect_err("old password").code(),
            code::INVALID_CREDENTIALS
        );
    }

    #[test]
    fn store_file_has_no_plaintext_password_or_session_token() {
        let dir = TempDir::new("no-plaintext");
        let mut auth = store(&dir);
        let issued = auth.setup(PASSWORD, PASSWORD).expect("setup");
        let text = std::fs::read_to_string(dir.config().auth_path()).expect("auth.json");
        assert!(!text.contains(PASSWORD), "auth.json 不能存明文密码");
        assert!(
            !text.contains(&issued.token),
            "auth.json 不能存裸会话 token"
        );
        assert!(
            text.contains("$argon2id$"),
            "密码必须是 argon2id PHC: {text}"
        );
        assert!(
            text.contains(&issued.csrf),
            "CSRF 随会话持久化（浏览器端也需要它）"
        );
    }

    #[test]
    fn expired_sessions_do_not_resolve() {
        let dir = TempDir::new("expiry");
        let config = dir.config().with_session_ttl_secs(0);
        let mut auth = AuthStore::load(config).expect("load");
        let issued = auth.setup(PASSWORD, PASSWORD).expect("setup");
        assert!(auth.resolve(&issued.token).is_none());
    }

    #[test]
    fn rfc3339_formats_utc_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339_utc(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn production_defaults_match_plan() {
        let config = AuthConfig::with_state_dir("unused");
        assert_eq!(config.params.m_cost(), 19_456);
        assert_eq!(config.params.t_cost(), 2);
        assert_eq!(config.params.p_cost(), 1);
        assert_eq!(config.session_ttl_secs, 8 * 60 * 60);
        assert_eq!(MIN_PASSWORD_LEN, 8);
    }

    #[test]
    fn parse_store_rejects_corrupt_files() {
        assert!(parse_store(b"not json").is_err());
        assert!(parse_store(br#"{"version": 2, "password": null, "sessions": []}"#).is_err());
        assert!(parse_store(
            br#"{"version": 1, "password": "plain-text-password", "sessions": []}"#
        )
        .is_err());
        assert!(parse_store(br#"{"version": 1, "password": null}"#).is_err());
        let good = parse_store(
            br#"{"version": 1, "password": null, "sessions": [
                {"token_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                 "csrf": "1111111111111111111111111111111111111111111111111111111111111111",
                 "created": 1, "expires": 2}]}"#,
        )
        .expect("valid store");
        assert!(good.0.is_none());
        assert_eq!(good.1.len(), 1);
    }
}
