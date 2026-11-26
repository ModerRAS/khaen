//! 新增 REST 端点共用的 M1 错误体（docs/API.md §11-A：`{error:{code,message,details}}`）。
//!
//! 只被本会话新增的端点使用（audio-profile / speaker-test / device-inputs）；既有端点保持原样。
//! HTTP 状态由调用方给（400 bad_request / 403 sound_window_closed / 409 busy 或 token 类 /
//! 503 not_wired 或 host_link_unavailable / 500 internal_error），code 是稳定机器码。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

/// 一条 M1 错误（状态 + 稳定码 + 人读说明 + 结构化 details）。
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Map<String, Value>,
}

impl ApiError {
    /// 构造（details 为空对象）。
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    /// 追加一个结构化字段（链式；重复 key 覆盖）。
    pub fn with_detail(mut self, key: &str, value: Value) -> Self {
        self.details.insert(key.to_string(), value);
        self
    }

    /// 请求字段缺失/类型不对/取值越界（400 bad_request）。
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// 点名具体字段的 400（details.field）。
    pub fn invalid_field(field: &str, message: impl Into<String>) -> Self {
        Self::bad_request(message).with_detail("field", json!(field))
    }

    /// 409 冲突（token 无效/busy/数据面拒绝）。
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// 503 未接线（后端/rt-host 没有这条能力；不假成功）。
    pub fn not_wired(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    /// HTTP 状态（单元测试断言用）。
    #[cfg(test)]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// 稳定错误码（单元测试断言用）。
    #[cfg(test)]
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 500 内部错误。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "details": Value::Object(self.details),
            }
        });
        (self.status, Json(body)).into_response()
    }
}

/// 读取请求体并按 JSON 解析（空体视为 `{}`；超限/非 JSON 都是 400）。
///
/// 控制面请求都很小，统一上限 64 KiB（与 host 行协议一致）。
pub async fn read_json_body(body: axum::body::Body) -> Result<Map<String, Value>, ApiError> {
    const MAX_BODY: usize = 64 * 1024;
    let bytes = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|error| ApiError::bad_request(format!("cannot read request body: {error}")))?;
    if bytes.is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ApiError::bad_request(format!("request body is not valid JSON: {error}"))
    })?;
    match value {
        Value::Object(object) => Ok(object),
        other => Err(ApiError::bad_request(format!(
            "request body must be a JSON object (got {})",
            json_kind(&other)
        ))),
    }
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
