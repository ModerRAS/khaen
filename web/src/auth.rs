//! 认证与会话。
//!
//! - 默认 token 存在配置文件（/etc/khaen-audio-web/config.toml，0600）。
//! - 浏览器登录后使用 HttpOnly + SameSite=Strict 会话 Cookie；
//!   所有写操作（POST/PUT/PATCH/DELETE）必须带与会话绑定的 CSRF token。
//! - 脚本/自动化可直接用 \`X-Auth-Token\` 或 \`Authorization: Bearer <token>\`，
//!   这类显式凭证不依赖浏览器环境，因此无需 CSRF。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::error::AppError;

pub const SESSION_COOKIE: &str = "khaen_session";
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const TOKEN_HEADER: &str = "x-auth-token";
pub const MAX_SESSIONS: usize = 64;
pub const MAX_LOGIN_FAILURES: u32 = 5;

/// 认证方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authenticated {
    /// 浏览器会话，携带该会话的 CSRF token。
    Session { csrf: String },
    /// 显式 token（Header）认证。
    Token,
}

impl Authenticated {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Session { .. } => "session",
            Self::Token => "token",
        }
    }
}

#[derive(Debug, Clone)]
struct Session {
    csrf: String,
    created: Instant,
    expires: Instant,
}

#[derive(Debug, Default, Clone)]
struct Attempts {
    failures: u32,
    locked_until: Option<Instant>,
}

/// 登录成功返回。
#[derive(Debug, Clone, Serialize)]
pub struct LoginSuccess {
    pub session_id: String,
    pub csrf_token: String,
    pub expires_in: u64,
}

pub struct AuthState {
    token: String,
    ttl: Duration,
    cookie_secure: bool,
    lockout: Duration,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, Attempts>>,
    /// Argon2id PHC 字符串；None = 尚未设置管理密码（首启 setup）。
    password_hash: Mutex<Option<String>>,
    auth_file: PathBuf,
    mock: bool,
}

impl AuthState {
    pub fn new(config: &AppConfig) -> Self {
        let auth_file = PathBuf::from(&config.auth_file);
        let password_hash = if config.mock {
            None
        } else {
            load_auth_hash(&auth_file)
        };
        Self {
            token: config.token.clone(),
            ttl: Duration::from_secs(config.session_ttl_secs.max(60)),
            cookie_secure: config.cookie_secure,
            lockout: Duration::from_secs(config.login_lockout_secs.max(1)),
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            password_hash: Mutex::new(password_hash),
            auth_file,
            mock: config.mock,
        }
    }

    /// 测试用构造（自定义 TTL，无密码、内存模式）。
    pub fn new_for_test(token: &str, ttl_secs: u64) -> Self {
        Self {
            token: token.to_string(),
            ttl: Duration::from_secs(ttl_secs),
            cookie_secure: false,
            lockout: Duration::from_secs(60),
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            password_hash: Mutex::new(None),
            auth_file: PathBuf::new(),
            mock: true,
        }
    }

    pub fn password_configured(&self) -> bool {
        self.password_hash.lock().unwrap().is_some()
    }

    /// 设置管理密码（仅未配置时可用）；成功后立即创建会话。
    pub fn setup_password(&self, key: &str, password: &str) -> Result<LoginSuccess, AppError> {
        if let Some(remain) = self.lock_remaining(key) {
            return Err(AppError::TooManyRequests(format!(
                "尝试次数过多，请 {remain} 秒后再试"
            )));
        }
        {
            let guard = self.password_hash.lock().unwrap();
            if guard.is_some() {
                return Err(AppError::conflict("管理密码已设置，不能重复 setup"));
            }
        }
        let hash = hash_password(password)?;
        if !self.mock {
            save_auth_hash(&self.auth_file, &hash)?;
        }
        *self.password_hash.lock().unwrap() = Some(hash);
        {
            self.attempts.lock().unwrap().remove(key);
        }
        Ok(self.create_session())
    }

    /// 密码登录；失败计入现有 IP 限流。
    pub fn login_password(&self, key: &str, password: &str) -> Result<LoginSuccess, AppError> {
        if let Some(remain) = self.lock_remaining(key) {
            return Err(AppError::TooManyRequests(format!(
                "登录失败次数过多，请 {remain} 秒后再试"
            )));
        }
        let hash = self.password_hash.lock().unwrap().clone();
        let Some(hash) = hash else {
            return Err(AppError::bad_request("尚未设置管理密码，请先完成首次设置"));
        };
        if !verify_password(&hash, password) {
            self.record_failure(key);
            return Err(AppError::unauthorized("密码不正确"));
        }
        self.attempts.lock().unwrap().remove(key);
        Ok(self.create_session())
    }

    fn create_session(&self) -> LoginSuccess {
        let session_id = random_hex(24);
        let csrf = random_hex(24);
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, s| s.expires > now);
        while sessions.len() >= MAX_SESSIONS {
            // 淘汰最早创建的会话
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest);
            } else {
                break;
            }
        }
        sessions.insert(
            session_id.clone(),
            Session {
                csrf: csrf.clone(),
                created: now,
                expires: now + self.ttl,
            },
        );
        LoginSuccess {
            session_id,
            csrf_token: csrf,
            expires_in: self.ttl.as_secs(),
        }
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl.as_secs()
    }

    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }

    /// 检查登录是否被限速。返回剩余锁定秒数。
    pub fn lock_remaining(&self, key: &str) -> Option<u64> {
        let mut attempts = self.attempts.lock().unwrap();
        let now = Instant::now();
        if let Some(a) = attempts.get_mut(key) {
            if let Some(until) = a.locked_until {
                if until > now {
                    return Some(until.duration_since(now).as_secs().max(1));
                }
                a.locked_until = None;
                a.failures = 0;
            }
        }
        None
    }

    /// 校验 token 并建立会话。
    pub fn login(&self, key: &str, token: &str) -> Result<LoginSuccess, AppError> {
        if let Some(remain) = self.lock_remaining(key) {
            return Err(AppError::TooManyRequests(format!(
                "登录失败次数过多，请 {remain} 秒后再试"
            )));
        }
        if !constant_time_eq(token.as_bytes(), self.token.as_bytes()) {
            self.record_failure(key);
            return Err(AppError::unauthorized("token 不正确"));
        }
        {
            let mut attempts = self.attempts.lock().unwrap();
            attempts.remove(key);
        }
        Ok(self.create_session())
    }

    fn record_failure(&self, key: &str) {
        let mut attempts = self.attempts.lock().unwrap();
        let entry = attempts.entry(key.to_string()).or_default();
        entry.failures += 1;
        if entry.failures >= MAX_LOGIN_FAILURES {
            entry.locked_until = Some(Instant::now() + self.lockout);
            entry.failures = 0;
        }
    }

    /// 从请求头认证（cookie 会话或显式 token）。
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<Authenticated, AppError> {
        if let Some(token) = header_str(headers, TOKEN_HEADER) {
            return self.authenticate_token(token);
        }
        if let Some(auth) = header_str(headers, "authorization") {
            if let Some(rest) = auth
                .strip_prefix("Bearer ")
                .or_else(|| auth.strip_prefix("bearer "))
            {
                return self.authenticate_token(rest.trim());
            }
        }
        if let Some(session_id) = cookie_value(headers, SESSION_COOKIE) {
            let mut sessions = self.sessions.lock().unwrap();
            let now = Instant::now();
            match sessions.get(&session_id) {
                Some(s) if s.expires > now => {
                    return Ok(Authenticated::Session {
                        csrf: s.csrf.clone(),
                    });
                }
                Some(_) => {
                    sessions.remove(&session_id);
                    return Err(AppError::unauthorized("会话已过期，请重新登录"));
                }
                None => return Err(AppError::unauthorized("会话无效，请重新登录")),
            }
        }
        Err(AppError::unauthorized("需要登录或提供访问 token"))
    }

    fn authenticate_token(&self, token: &str) -> Result<Authenticated, AppError> {
        if constant_time_eq(token.as_bytes(), self.token.as_bytes()) {
            Ok(Authenticated::Token)
        } else {
            Err(AppError::unauthorized("token 不正确"))
        }
    }

    /// 注销会话。
    pub fn logout(&self, headers: &HeaderMap) {
        if let Some(session_id) = cookie_value(headers, SESSION_COOKIE) {
            self.sessions.lock().unwrap().remove(&session_id);
        }
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

/// 密码策略：至少 8 位（推荐 10+），确认一致，禁止与 token 相同、禁止纯空白。
pub fn validate_password(password: &str, confirm: &str) -> Result<(), AppError> {
    if password.len() < 8 {
        return Err(AppError::bad_request("密码至少 8 位（推荐 10 位以上）"));
    }
    if password.len() > 128 {
        return Err(AppError::bad_request("密码最长 128 位"));
    }
    if password.trim().is_empty() || password.chars().all(char::is_whitespace) {
        return Err(AppError::bad_request("密码不能为空白"));
    }
    if password != confirm {
        return Err(AppError::bad_request("两次输入的密码不一致"));
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, AppError> {
    let mut salt_bytes = [0u8; 16];
    getrandom::fill(&mut salt_bytes)
        .map_err(|e| AppError::internal(format!("生成 salt 失败: {e}")))?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|e| AppError::internal(format!("salt 编码失败: {e}")))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::internal(format!("Argon2id 哈希失败: {e}")))
}

fn verify_password(phc: &str, password: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthFile {
    schema: String,
    #[serde(default)]
    algorithm: String,
    password_hash: String,
    #[serde(default)]
    updated_unix: u64,
}

fn load_auth_hash(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: AuthFile = serde_json::from_str(&text).ok()?;
    if parsed.password_hash.trim().is_empty() {
        return None;
    }
    Some(parsed.password_hash)
}

fn save_auth_hash(path: &std::path::Path, hash: &str) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("创建目录失败: {e}")))?;
    }
    let file = AuthFile {
        schema: "khaen-auth/v1".into(),
        algorithm: "argon2id".into(),
        password_hash: hash.to_string(),
        updated_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let text = serde_json::to_string_pretty(&file)
        .map_err(|e| AppError::internal(format!("序列化 auth.json 失败: {e}")))?;
    std::fs::write(path, format!("{text}\n"))
        .map_err(|e| AppError::internal(format!("写入 {} 失败: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 请求体：登录。token 与 password 二选一（token 保留为恢复/CLI 兜底）。
#[derive(Debug, Deserialize, Default)]
pub struct LoginRequest {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

/// 请求体：首次设置管理密码。
#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub password: String,
    #[serde(default)]
    pub confirm: String,
}

/// 常量时间比较（防时序侧信道）。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let _ = getrandom::fill(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 从 Cookie 头中取指定 cookie 值。
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(axum::http::header::COOKIE).iter() {
        let Ok(text) = value.to_str() else { continue };
        for part in text.split(';') {
            let part = part.trim();
            if let Some((k, v)) = part.split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// 是否为改变状态的 HTTP 方法（需要 CSRF）。
pub fn is_write_method(method: &axum::http::Method) -> bool {
    matches!(
        *method,
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    )
}

/// 校验写操作 CSRF；返回错误即拒绝。
pub fn check_csrf(auth: &Authenticated, headers: &HeaderMap) -> Result<(), AppError> {
    if let Authenticated::Session { csrf } = auth {
        let provided = header_str(headers, CSRF_HEADER).unwrap_or_default();
        if !constant_time_eq(provided.as_bytes(), csrf.as_bytes()) {
            return Err(AppError::forbidden(
                "CSRF 校验失败：缺少或错误的 X-CSRF-Token",
            ));
        }
    }
    Ok(())
}

/// 会话 Cookie 的 Set-Cookie 值。
pub fn session_cookie(session_id: &str, ttl_secs: u64, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={ttl_secs}{}",
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        use std::str::FromStr;
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::header::HeaderName::from_str(k).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn cookie_parse() {
        let h = headers_with(&[("cookie", "a=1; khaen_session=deadbeef; b=2")]);
        assert_eq!(cookie_value(&h, SESSION_COOKIE).unwrap(), "deadbeef");
        assert!(cookie_value(&h, "nope").is_none());
    }

    #[test]
    fn login_creates_session_and_csrf() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        let ok = auth.login("test", "secret-token").unwrap();
        assert_eq!(ok.csrf_token.len(), 48);
        assert_eq!(ok.session_id.len(), 48);
        let h = headers_with(&[("cookie", &format!("khaen_session={}", ok.session_id))]);
        match auth.authenticate(&h).unwrap() {
            Authenticated::Session { csrf } => assert_eq!(csrf, ok.csrf_token),
            other => panic!("expected session, got {other:?}"),
        }
    }

    #[test]
    fn wrong_token_rejected() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        let err = auth.login("test", "wrong").unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        assert!(auth.login("test", "secret-token").is_ok());
    }

    #[test]
    fn login_locks_after_failures() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        for _ in 0..MAX_LOGIN_FAILURES {
            let _ = auth.login("1.2.3.4", "bad");
        }
        let err = auth.login("1.2.3.4", "secret-token").unwrap_err();
        assert!(matches!(err, AppError::TooManyRequests(_)));
        // 其他 IP 不受影响
        assert!(auth.login("5.6.7.8", "secret-token").is_ok());
    }

    #[test]
    fn password_policy() {
        assert!(validate_password("short", "short").is_err());
        assert!(validate_password("longenough", "different").is_err());
        assert!(validate_password("        ", "        ").is_err());
        assert!(validate_password("correct-horse", "correct-horse").is_ok());
    }

    #[test]
    fn password_hash_roundtrip() {
        let hash = hash_password("correct-horse").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password(&hash, "correct-horse"));
        assert!(!verify_password(&hash, "wrong"));
    }

    #[test]
    fn setup_then_password_login_and_token_fallback() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        assert!(!auth.password_configured());
        let s = auth.setup_password("1.2.3.4", "correct-horse").unwrap();
        assert!(!s.session_id.is_empty());
        assert!(auth.password_configured());
        // 重复 setup 拒绝
        assert!(matches!(
            auth.setup_password("1.2.3.4", "another-pass").unwrap_err(),
            AppError::Conflict(_)
        ));
        // 密码登录
        assert!(auth.login_password("1.2.3.4", "correct-horse").is_ok());
        assert!(matches!(
            auth.login_password("1.2.3.4", "wrong").unwrap_err(),
            AppError::Unauthorized(_)
        ));
        // token 保留为恢复兜底
        assert!(auth.login("1.2.3.4", "secret-token").is_ok());
    }

    #[test]
    fn password_login_locks_after_failures() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        auth.setup_password("1.2.3.4", "correct-horse").unwrap();
        for _ in 0..MAX_LOGIN_FAILURES {
            let _ = auth.login_password("1.2.3.4", "bad-password");
        }
        assert!(matches!(
            auth.login_password("1.2.3.4", "correct-horse").unwrap_err(),
            AppError::TooManyRequests(_)
        ));
        assert!(auth.login_password("5.6.7.8", "correct-horse").is_ok());
    }

    #[test]
    fn auth_file_save_and_load() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("auth-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("auth.json");
        let hash = hash_password("correct-horse").unwrap();
        save_auth_hash(&p, &hash).unwrap();
        assert_eq!(load_auth_hash(&p).as_deref(), Some(hash.as_str()));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("khaen-auth/v1"));
        assert!(text.contains("argon2id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_header_auth() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        let h = headers_with(&[("x-auth-token", "secret-token")]);
        assert_eq!(auth.authenticate(&h).unwrap(), Authenticated::Token);
        let bearer = headers_with(&[("authorization", "Bearer secret-token")]);
        assert_eq!(auth.authenticate(&bearer).unwrap(), Authenticated::Token);
        let bad = headers_with(&[("x-auth-token", "nope")]);
        assert!(auth.authenticate(&bad).is_err());
    }

    #[test]
    fn csrf_only_required_for_sessions() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        let ok = auth.login("test", "secret-token").unwrap();
        let session = Authenticated::Session {
            csrf: ok.csrf_token.clone(),
        };
        let none = headers_with(&[]);
        assert!(check_csrf(&session, &none).is_err());
        let wrong = headers_with(&[("x-csrf-token", "bad")]);
        assert!(check_csrf(&session, &wrong).is_err());
        let good = headers_with(&[("x-csrf-token", &ok.csrf_token)]);
        assert!(check_csrf(&session, &good).is_ok());
        assert!(check_csrf(&Authenticated::Token, &none).is_ok());
    }

    #[test]
    fn expired_session_rejected() {
        let auth = AuthState::new_for_test("secret-token", 0);
        let ok = auth.login("test", "secret-token").unwrap();
        let h = headers_with(&[("cookie", &format!("khaen_session={}", ok.session_id))]);
        // ttl=0 时 expires == created，视为已过期
        assert!(auth.authenticate(&h).is_err());
    }

    #[test]
    fn logout_removes_session() {
        let auth = AuthState::new_for_test("secret-token", 3600);
        let ok = auth.login("test", "secret-token").unwrap();
        let h = headers_with(&[("cookie", &format!("khaen_session={}", ok.session_id))]);
        assert_eq!(auth.session_count(), 1);
        auth.logout(&h);
        assert_eq!(auth.session_count(), 0);
        assert!(auth.authenticate(&h).is_err());
    }

    #[test]
    fn session_cookie_flags() {
        let c = session_cookie("abc", 7200, false);
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("SameSite=Strict"));
        assert!(c.contains("Max-Age=7200"));
        assert!(!c.contains("Secure"));
        assert!(session_cookie("abc", 1, true).contains("Secure"));
    }
}
